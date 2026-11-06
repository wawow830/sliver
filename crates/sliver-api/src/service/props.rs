//! Converting item properties to and from D-Bus values.

use super::*;

/// zvariant → [`Prop`]. Unsupported types (fds, structs of mixed types) become `None`.
pub fn prop_from_value(v: &Value<'_>) -> Option<Prop> {
    Some(match v {
        Value::Bool(b) => Prop::Bool(*b),
        Value::U8(x) => Prop::Int((*x).into()),
        Value::I16(x) => Prop::Int((*x).into()),
        Value::U16(x) => Prop::Int((*x).into()),
        Value::I32(x) => Prop::Int((*x).into()),
        Value::U32(x) => Prop::Int((*x).into()),
        Value::I64(x) => Prop::Int(*x),
        Value::U64(x) => Prop::Int(i64::try_from(*x).ok()?),
        Value::F64(x) => Prop::Num(*x),
        Value::Str(s) => Prop::Str(s.to_string()),
        Value::ObjectPath(p) => Prop::Str(p.to_string()),
        Value::Signature(s) => Prop::Str(s.to_string()),
        Value::Value(inner) => return prop_from_value(inner),
        Value::Array(a) if a.element_signature() == &Signature::U8 => Prop::Bytes(
            a.inner()
                .iter()
                .filter_map(|x| if let Value::U8(b) = x { Some(*b) } else { None })
                .collect(),
        ),
        Value::Array(a) => Prop::List(a.inner().iter().filter_map(prop_from_value).collect()),
        Value::Dict(d) => Prop::Map(
            d.iter()
                .filter_map(|(k, v)| match k {
                    Value::Str(k) => Some((k.to_string(), prop_from_value(v)?)),
                    _ => None,
                })
                .collect(),
        ),
        Value::Structure(s) => Prop::List(s.fields().iter().filter_map(prop_from_value).collect()),
        _ => return None,
    })
}

pub(super) fn to_props(m: &HashMap<String, OwnedValue>) -> Props {
    m.iter()
        .filter_map(|(k, v)| Some((k.clone(), prop_from_value(v)?)))
        .collect()
}

/// [`Prop`] → zvariant. Ints become `i` when they fit, else `x`. Lists of mixed types become `av`.
pub fn prop_to_value(p: &Prop) -> Value<'static> {
    match p {
        Prop::Bool(b) => Value::Bool(*b),
        Prop::Int(i) => i32::try_from(*i).map(Value::I32).unwrap_or(Value::I64(*i)),
        Prop::Num(n) => Value::F64(*n),
        Prop::Str(s) => Value::Str(Str::from(s.clone())),
        Prop::Bytes(b) => Value::from(b.clone()),
        Prop::List(l) => {
            let mut a = Array::new(&Signature::Variant);
            for x in l {
                let _ = a.append(Value::Value(Box::new(prop_to_value(x))));
            }
            Value::Array(a)
        }
        Prop::Map(m) => Value::Dict(to_dict_value(m)),
    }
}

pub(super) fn to_dict_value(m: &Props) -> Dict<'static, 'static> {
    let mut d = Dict::new(&Signature::Str, &Signature::Variant);
    for (k, v) in m {
        let _ = d.append(
            Value::Str(Str::from(k.clone())),
            Value::Value(Box::new(prop_to_value(v))),
        );
    }
    d
}

pub(super) fn to_dict(m: &Props) -> HashMap<String, Value<'static>> {
    m.iter()
        .map(|(k, v)| (k.clone(), prop_to_value(v)))
        .collect()
}

pub(super) fn to_owned_dict(m: &Props) -> HashMap<String, OwnedValue> {
    m.iter()
        .filter_map(|(k, v)| Some((k.clone(), OwnedValue::try_from(prop_to_value(v)).ok()?)))
        .collect()
}
