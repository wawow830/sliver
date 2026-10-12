//! `{ dbus = { dest, path, interface, method, args } }` with arguments.
//!
//! TOML has fewer types than D-Bus (one integer type, no unsigned, no object paths), so the
//! arguments are coerced to what the method really takes. That is read from the target's own
//! introspection ([`in_signatures`]); when it can't be introspected, the types are guessed from
//! the TOML values ([`guess`]): bool → `b`, integer → `i` (`x` when too big), float → `d`,
//! string → `s`, array → array of the first element's type, table → `a{sv}`.

use std::str::FromStr;
use toml::Value as T;
use zbus::zvariant::{Array, Dict, ObjectPath, Signature, Structure, StructureBuilder, Value};

/// The `in` argument types of `method` from an `Introspect()` XML document. `None` when the
/// interface or method isn't in it.
pub fn in_signatures(xml: &str, interface: &str, method: &str) -> Option<Vec<Signature>> {
    let attr = |tag: &str, name: &str| -> Option<String> {
        let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        let rest = &tag[at..];
        Some(rest[..rest.find('"')?].to_string())
    };
    let mut in_iface = false;
    let mut in_method = false;
    let mut out = Vec::new();
    let mut found = false;
    for tag in xml.split('<').skip(1) {
        let tag = tag.split('>').next().unwrap_or("");
        if tag.starts_with("interface ") {
            in_iface = attr(tag, "name").as_deref() == Some(interface);
        } else if tag.starts_with("/interface") {
            in_iface = false;
        } else if in_iface && tag.starts_with("method ") {
            in_method = attr(tag, "name").as_deref() == Some(method);
            found |= in_method;
            // `<method name="X"/>` has no arguments and no closing tag.
            if tag.ends_with('/') {
                in_method = false;
            }
        } else if tag.starts_with("/method") {
            in_method = false;
        } else if in_method && tag.starts_with("arg ") {
            // direction defaults to "in" for methods
            if attr(tag, "direction").as_deref().unwrap_or("in") == "in" {
                out.push(Signature::from_str(&attr(tag, "type")?).ok()?);
            }
        }
    }
    found.then_some(out)
}

/// A D-Bus type for a TOML value that has no declared one.
pub fn guess(v: &T) -> Signature {
    match v {
        T::Boolean(_) => Signature::Bool,
        T::Integer(i) if i32::try_from(*i).is_ok() => Signature::I32,
        T::Integer(_) => Signature::I64,
        T::Float(_) => Signature::F64,
        T::Array(a) => Signature::Array(a.first().map_or(Signature::Str, guess).into()),
        T::Table(_) => Signature::Dict {
            key: Signature::Str.into(),
            value: Signature::Variant.into(),
        },
        _ => Signature::Str,
    }
}

fn int(v: &T) -> Result<i64, String> {
    match v {
        T::Integer(i) => Ok(*i),
        other => Err(format!("expected an integer, got {}", other.type_str())),
    }
}

/// `v` as a value of type `sig`.
pub fn coerce(sig: &Signature, v: &T) -> Result<Value<'static>, String> {
    fn num<N: TryFrom<i64>>(v: &T, what: &str) -> Result<N, String> {
        let i = int(v)?;
        N::try_from(i).map_err(|_| format!("{i} does not fit {what}"))
    }
    Ok(match sig {
        Signature::Bool => match v {
            T::Boolean(b) => Value::Bool(*b),
            other => return Err(format!("expected true/false, got {}", other.type_str())),
        },
        Signature::U8 => Value::U8(num(v, "a byte")?),
        Signature::I16 => Value::I16(num(v, "int16")?),
        Signature::U16 => Value::U16(num(v, "uint16")?),
        Signature::I32 => Value::I32(num(v, "int32")?),
        Signature::U32 => Value::U32(num(v, "uint32")?),
        Signature::I64 => Value::I64(int(v)?),
        Signature::U64 => Value::U64(num(v, "uint64")?),
        Signature::F64 => match v {
            T::Float(f) => Value::F64(*f),
            T::Integer(i) => Value::F64(*i as f64),
            other => return Err(format!("expected a number, got {}", other.type_str())),
        },
        Signature::Str => match v {
            T::String(s) => Value::from(s.clone()),
            other => return Err(format!("expected a string, got {}", other.type_str())),
        },
        Signature::ObjectPath => match v {
            T::String(s) => Value::ObjectPath(
                ObjectPath::try_from(s.clone())
                    .map_err(|e| format!("bad object path {s:?}: {e}"))?,
            ),
            other => return Err(format!("expected a path string, got {}", other.type_str())),
        },
        Signature::Variant => Value::Value(Box::new(coerce(&guess(v), v)?)),
        Signature::Array(child) => {
            let T::Array(items) = v else {
                return Err(format!("expected an array, got {}", v.type_str()));
            };
            let mut a = Array::new(child);
            for i in items {
                a.append(coerce(child, i)?).map_err(|e| e.to_string())?;
            }
            Value::Array(a)
        }
        Signature::Dict { key, value } => {
            let T::Table(t) = v else {
                return Err(format!("expected a table, got {}", v.type_str()));
            };
            if **key != Signature::Str {
                return Err("only string-keyed dictionaries are supported".into());
            }
            let mut d = Dict::new(key, value);
            for (k, x) in t {
                d.append(Value::from(k.clone()), coerce(value, x)?)
                    .map_err(|e| e.to_string())?;
            }
            Value::Dict(d)
        }
        other => return Err(format!("unsupported argument type {other}")),
    })
}

/// All the arguments as a body (at least one: a call without arguments sends `()` instead). `types` are the method's declared `in` types (empty = guess).
pub fn body(types: &[Signature], args: &[T]) -> Result<Structure<'static>, String> {
    if !types.is_empty() && types.len() != args.len() {
        return Err(format!(
            "the method takes {} argument(s), the config has {}",
            types.len(),
            args.len()
        ));
    }
    let mut b = StructureBuilder::new();
    for (i, a) in args.iter().enumerate() {
        let sig = types.get(i).cloned().unwrap_or_else(|| guess(a));
        b = b.append_field(coerce(&sig, a).map_err(|e| format!("argument {}: {e}", i + 1))?);
    }
    b.build().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<node>
 <interface name="org.freedesktop.DBus.Peer"><method name="Ping"/></interface>
 <interface name="org.freedesktop.login1.Manager">
  <method name="Suspend"><arg type="b" direction="in"/></method>
  <method name="GetSession"><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
  <method name="Idle"/>
  <method name="Mix"><arg name="n" type="u" direction="in"/><arg type="a{sv}" direction="in"/><arg type="as" direction="in"/></method>
 </interface></node>"#;

    fn sigs(s: &[&str]) -> Vec<Signature> {
        s.iter().map(|x| Signature::from_str(x).unwrap()).collect()
    }

    #[test]
    fn reads_in_arguments_from_introspection() {
        let i = "org.freedesktop.login1.Manager";
        assert_eq!(in_signatures(XML, i, "Suspend"), Some(sigs(&["b"])));
        assert_eq!(
            in_signatures(XML, i, "GetSession"),
            Some(sigs(&["s"])),
            "out args skipped"
        );
        assert_eq!(in_signatures(XML, i, "Idle"), Some(vec![]));
        assert_eq!(
            in_signatures(XML, i, "Mix"),
            Some(sigs(&["u", "a{sv}", "as"]))
        );
        assert_eq!(in_signatures(XML, i, "Nope"), None);
        assert_eq!(in_signatures(XML, "other.Iface", "Suspend"), None);
    }

    fn t(s: &str) -> T {
        format!("v = {s}").parse::<toml::Table>().unwrap()["v"].clone()
    }

    #[test]
    fn coerces_to_the_declared_types() {
        let u = Signature::U32;
        assert_eq!(coerce(&u, &t("5")).unwrap(), Value::U32(5));
        assert!(coerce(&u, &t("-1")).unwrap_err().contains("does not fit"));
        assert!(coerce(&Signature::Bool, &t("1")).is_err());
        assert_eq!(coerce(&Signature::F64, &t("2")).unwrap(), Value::F64(2.0));
        assert!(matches!(
            coerce(&Signature::ObjectPath, &t("\"/a/b\"")).unwrap(),
            Value::ObjectPath(_)
        ));
        assert!(coerce(&Signature::ObjectPath, &t("\"not a path\"")).is_err());
        let a = coerce(&Signature::from_str("as").unwrap(), &t("[\"x\", \"y\"]")).unwrap();
        assert_eq!(a.value_signature().to_string(), "as");
        let d = coerce(
            &Signature::from_str("a{sv}").unwrap(),
            &t("{ interactive = true, n = 3 }"),
        )
        .unwrap();
        assert_eq!(d.value_signature().to_string(), "a{sv}");
        assert!(coerce(&Signature::from_str("(ii)").unwrap(), &t("[1,2]")).is_err());
    }

    #[test]
    fn body_checks_arity_and_guesses_without_types() {
        let b = body(&sigs(&["b"]), &[t("false")]).unwrap();
        assert_eq!(b.fields().len(), 1);
        assert!(body(&sigs(&["b"]), &[]).unwrap_err().contains("takes 1"));
        // no introspection: guessed
        let g = body(&[], &[t("\"\""), t("{ interactive = true }"), t("[1, 2]")]).unwrap();
        let sig = g
            .fields()
            .iter()
            .map(|f| f.value_signature().to_string())
            .collect::<Vec<_>>();
        assert_eq!(sig, ["s", "a{sv}", "ai"]);
    }
}
