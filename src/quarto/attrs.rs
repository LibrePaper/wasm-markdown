//! The attribute parser `{#id .class key=value}`, shared by divs, spans and
//! cells.
//!
//! Owner: the scaffold (mww4). Feature workers read this module and do not
//! edit it; a missing capability is requested from the coordinator.

use super::front_matter::{scalar, set, Map};

/// What a `{...}` attribute block holds.
#[derive(Default, Debug, Clone)]
#[allow(dead_code)] // `classes` is read by the feature modules
pub(crate) struct Attrs {
    pub id: String,
    pub classes: Vec<String>,
    pub attrs: Map,
}

/// End of a quoted run starting at `i`, or none when it is never closed.
fn quoted_end(b: &[u8], i: usize, quote: u8) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        if quote == b'"' && b[j] == b'\\' {
            if j + 1 >= b.len() {
                return None;
            }
            j += 2;
        } else if b[j] == quote {
            if quote == b'\'' && j + 1 < b.len() && b[j + 1] == b'\'' {
                j += 2;
            } else {
                return Some(j + 1);
            }
        } else {
            j += 1;
        }
    }
    None
}

/// Words separated by spaces, where a quoted run belongs to its word.
fn tokens(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            if b[i] == b'"' || b[i] == b'\'' {
                match quoted_end(b, i, b[i]) {
                    Some(end) => i = end,
                    None => break,
                }
            } else {
                i += 1;
            }
        }
        if i > start {
            out.push(&text[start..i]);
        } else {
            i += 1; // a quote that never closes
        }
    }
    out
}

/// `{#id .class key=value}`: the id, the classes and the key-value pairs.
pub(crate) fn parse_attributes(input: &str) -> Attrs {
    let mut t = input.trim();
    t = t.strip_prefix('{').unwrap_or(t);
    t = t.strip_suffix('}').unwrap_or(t).trim();
    let mut out = Attrs::default();
    for token in tokens(t) {
        if let Some(id) = token.strip_prefix('#') {
            out.id = id.to_string();
        } else if let Some(class) = token.strip_prefix('.') {
            if !class.is_empty() {
                out.classes.push(class.to_string());
            }
        } else if let Some(at) = token.find('=') {
            if at > 0 {
                set(&mut out.attrs, &token[..at], scalar(&token[at + 1..]));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quarto::front_matter::Yaml;

    #[test]
    fn id_classes_and_pairs() {
        let a = parse_attributes("{#fig-a .wide .x key=\"two words\" n=3}");
        assert_eq!(a.id, "fig-a");
        assert_eq!(a.classes, ["wide", "x"]);
        assert_eq!(a.attrs[0], ("key".to_string(), Yaml::Str("two words".into())));
        assert_eq!(a.attrs[1], ("n".to_string(), Yaml::Num(3.0)));
    }

    #[test]
    fn a_bare_dot_is_not_a_class() {
        assert!(parse_attributes("{. }").classes.is_empty());
    }
}
