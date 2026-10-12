//! Selectors: `tag#id.class:pseudo`, descendant and child (`>`) combinators, selector lists.

/// One node in the style tree (`bar > zone > module`), as seen by selectors.
#[derive(Clone, Copy, Debug)]
pub struct Element<'a> {
    pub tag: &'a str,
    pub id: Option<&'a str>,
    pub classes: &'a [String],
    /// Pressed (`:active`).
    pub active: bool,
    pub disabled: bool,
    pub first: bool,
    pub last: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Pseudo {
    Active,
    Disabled,
    FirstChild,
    LastChild,
    Not(Box<Compound>),
    /// Valid CSS that can never match on a touch strip (`:hover`, `:focus`, …).
    Never,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Compound {
    tag: Option<String>,
    ids: Vec<String>,
    classes: Vec<String>,
    pseudos: Vec<Pseudo>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Combinator {
    Descendant,
    Child,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Selector {
    /// Rightmost first. `parts[i].0` is the combinator between `parts[i]` and `parts[i + 1]`.
    parts: Vec<(Combinator, Compound)>,
    pub specificity: (u16, u16, u16),
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()
}

fn ident(s: &str) -> (&str, &str) {
    let end = s.find(|c| !is_ident(c)).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// Parses one compound selector from the front of `s`. Returns it and the rest.
fn compound<'a>(mut s: &'a str, notes: &mut Vec<String>) -> Result<(Compound, &'a str), String> {
    let mut c = Compound::default();
    if let Some(rest) = s.strip_prefix('*') {
        s = rest;
    } else {
        let (tag, rest) = ident(s);
        if !tag.is_empty() {
            c.tag = Some(tag.to_ascii_lowercase());
            s = rest;
        }
    }
    while let Some(first) = s.chars().next() {
        match first {
            '#' | '.' => {
                let (name, rest) = ident(&s[1..]);
                if name.is_empty() {
                    return Err(format!("expected a name after '{first}'"));
                }
                if first == '#' {
                    c.ids.push(name.to_string());
                } else {
                    c.classes.push(name.to_string());
                }
                s = rest;
            }
            ':' => {
                let after = s[1..].strip_prefix(':').unwrap_or(&s[1..]);
                let (name, rest) = ident(after);
                s = rest;
                let pseudo = match name.to_ascii_lowercase().as_str() {
                    "active" => Pseudo::Active,
                    "checked" => {
                        return Err(
                            "':checked' isn't supported; toggles that are on have the class '.on'"
                                .into(),
                        );
                    }
                    "disabled" => Pseudo::Disabled,
                    "first-child" => Pseudo::FirstChild,
                    "last-child" => Pseudo::LastChild,
                    "not" => {
                        let inner = s
                            .strip_prefix('(')
                            .and_then(|r| r.split_once(')'))
                            .ok_or("expected ':not(...)'")?;
                        let (inner_c, left) = compound(inner.0.trim(), notes)?;
                        if !left.trim().is_empty() {
                            return Err(":not() takes one simple selector".into());
                        }
                        s = inner.1;
                        Pseudo::Not(Box::new(inner_c))
                    }
                    "hover" | "focus" | "focus-visible" | "focus-within" | "backdrop"
                    | "visited" | "link" => {
                        notes.push(format!("':{name}' never matches on the Touch Bar"));
                        Pseudo::Never
                    }
                    _ => return Err(format!("unsupported pseudo-class ':{name}'")),
                };
                c.pseudos.push(pseudo);
            }
            _ => break,
        }
    }
    Ok((c, s))
}

impl Selector {
    /// Parses one selector (no commas). `notes` collects non-fatal remarks.
    pub fn parse(text: &str, notes: &mut Vec<String>) -> Result<Selector, String> {
        let mut parts: Vec<(Combinator, Compound)> = Vec::new();
        let mut s = text.trim();
        let mut comb = Combinator::Descendant;
        while !s.is_empty() {
            let (c, rest) = compound(s, notes)?;
            if rest.len() == s.len() {
                return Err(format!("unexpected '{}'", s.chars().next().unwrap_or(' ')));
            }
            parts.push((comb, c));
            let trimmed = rest.trim_start();
            if let Some(r) = trimmed.strip_prefix('>') {
                comb = Combinator::Child;
                s = r.trim_start();
            } else if trimmed.starts_with(['+', '~']) {
                return Err("sibling combinators ('+', '~') are not supported".into());
            } else if trimmed.len() < rest.len() || trimmed.is_empty() {
                comb = Combinator::Descendant;
                s = trimmed;
            } else {
                return Err(format!(
                    "unexpected '{}'",
                    trimmed.chars().next().unwrap_or(' ')
                ));
            }
        }
        if parts.is_empty() {
            return Err("empty selector".into());
        }
        // Each compound carries the combinator to its left neighbour, so rightmost-first order
        // gives parts[i].0 = link from parts[i] to parts[i + 1].
        parts.reverse();
        let mut spec = (0, 0, 0);
        fn add(c: &Compound, spec: &mut (u16, u16, u16)) {
            spec.0 += c.ids.len() as u16;
            spec.1 += c.classes.len() as u16;
            spec.2 += c.tag.is_some() as u16;
            for p in &c.pseudos {
                match p {
                    Pseudo::Not(inner) => add(inner, spec),
                    _ => spec.1 += 1,
                }
            }
        }
        parts.iter().for_each(|(_, c)| add(c, &mut spec));
        Ok(Selector {
            parts,
            specificity: spec,
        })
    }

    /// Does the last element of `path` (root first) match?
    pub fn matches(&self, path: &[Element<'_>]) -> bool {
        match path.split_last() {
            Some((el, ancestors)) => self.match_from(0, el, ancestors),
            None => false,
        }
    }

    fn match_from(&self, i: usize, el: &Element<'_>, ancestors: &[Element<'_>]) -> bool {
        let (comb, c) = &self.parts[i];
        if !compound_matches(c, el) {
            return false;
        }
        if i + 1 == self.parts.len() {
            return true;
        }
        match comb {
            Combinator::Child => match ancestors.split_last() {
                Some((parent, rest)) => self.match_from(i + 1, parent, rest),
                None => false,
            },
            Combinator::Descendant => (0..ancestors.len())
                .rev()
                .any(|j| self.match_from(i + 1, &ancestors[j], &ancestors[..j])),
        }
    }
}

fn compound_matches(c: &Compound, el: &Element<'_>) -> bool {
    c.tag.as_deref().is_none_or(|t| t == el.tag)
        && c.ids.iter().all(|id| el.id == Some(id.as_str()))
        && c.classes
            .iter()
            .all(|cl| el.classes.iter().any(|x| x == cl))
        && c.pseudos.iter().all(|p| match p {
            Pseudo::Active => el.active,
            Pseudo::Disabled => el.disabled,
            Pseudo::FirstChild => el.first,
            Pseudo::LastChild => el.last,
            Pseudo::Not(inner) => !compound_matches(inner, el),
            Pseudo::Never => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el<'a>(tag: &'a str, id: Option<&'a str>, classes: &'a [String]) -> Element<'a> {
        Element {
            tag,
            id,
            classes,
            active: false,
            disabled: false,
            first: false,
            last: false,
        }
    }

    fn sel(s: &str) -> Selector {
        Selector::parse(s, &mut Vec::new()).unwrap()
    }

    #[test]
    fn matching_and_specificity() {
        let fnl = vec!["layer-fn".to_string()];
        let crit = vec!["critical".to_string(), "bat2".to_string()];
        let none: Vec<String> = vec![];
        let bar = el("bar", None, &fnl);
        let zone = el("zone", Some("main"), &none);
        let mut module = el("module", Some("battery"), &crit);
        let path = [bar, zone, module];

        assert!(sel("module").matches(&path));
        assert!(sel("*").matches(&path));
        assert!(sel("#battery.critical").matches(&path));
        assert!(sel("bar.layer-fn #battery").matches(&path));
        assert!(sel("zone#main > module").matches(&path));
        assert!(!sel("bar > module").matches(&path));
        assert!(!sel("#battery.warning").matches(&path));
        assert!(sel("#battery:not(.warning)").matches(&path));
        assert!(!sel("module:active").matches(&path));
        module.active = true;
        module.last = true;
        assert!(sel("module:active:last-child").matches(&[bar, zone, module]));

        assert_eq!(
            sel("bar.layer-fn #battery.critical:active").specificity,
            (1, 3, 1)
        );
        assert_eq!(sel("#a:not(#b)").specificity, (2, 0, 0));
    }

    #[test]
    fn errors_and_notes() {
        let mut notes = Vec::new();
        assert!(Selector::parse("a + b", &mut notes).is_err());
        assert!(Selector::parse("a:wiggle", &mut notes).is_err());
        assert!(Selector::parse("a,", &mut notes).is_err());
        let s = Selector::parse("module:hover", &mut notes).unwrap();
        assert_eq!(notes, ["':hover' never matches on the Touch Bar"]);
        let none: Vec<String> = vec![];
        assert!(!s.matches(&[el("module", None, &none)]));
    }
}
