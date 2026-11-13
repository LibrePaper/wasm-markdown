//! Conditional content: `.content-visible` and `.content-hidden`.
//!
//! A div or span with one of those classes and `when-format`, `unless-format`,
//! `when-meta` or `unless-meta` shows or hides its content. This renderer is
//! the `html` format (`pdf` for the draft Pandoc reads). Hidden divs are
//! blanked out of the source lines before anything is numbered, so a hidden
//! figure takes no number and a hidden heading is not in the contents; the
//! fences of a visible div are blanked too, its content is ordinary.

use super::front_matter::{get, Map, Yaml};

/// Whether the content with these classes and attributes is shown, or none
/// when it is not conditional.
pub(crate) fn visibility(
    classes: &[String],
    attrs: &Map,
    meta: &Map,
    for_pandoc: bool,
) -> Option<bool> {
    let visible_when_matching = if classes.iter().any(|c| c == "content-visible") {
        true
    } else if classes.iter().any(|c| c == "content-hidden") {
        false
    } else {
        return None;
    };
    Some(matches(attrs, meta, for_pandoc) == visible_when_matching)
}

/// Quarto's `quarto.format.is_format` for the format this renderer is.
fn format_is(wanted: &str, for_pandoc: bool) -> bool {
    if for_pandoc {
        matches!(wanted, "pdf" | "latex")
    } else {
        matches!(wanted, "html" | "html:js")
    }
}

/// A dotted key of the front matter is exactly boolean true.
fn meta_is_true(meta: &Map, key: &str) -> bool {
    let mut parts = key.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    let mut current = get(meta, first);
    for part in parts {
        current = match current {
            Some(Yaml::Map(m)) => get(m, part),
            _ => None,
        };
    }
    matches!(current, Some(Yaml::Bool(true)))
}

/// Every condition present must hold (`unless-` ones inverted).
fn matches(attrs: &Map, meta: &Map, for_pandoc: bool) -> bool {
    let tests: [(&str, bool); 4] = [
        ("when-meta", false),
        ("unless-meta", true),
        ("when-format", false),
        ("unless-format", true),
    ];
    for (key, invert) in tests {
        let Some(value) = get(attrs, key) else {
            continue;
        };
        let values: Vec<String> = match value {
            Yaml::List(items) => items.iter().map(Yaml::text).collect(),
            other => vec![other.text()],
        };
        let hit = values.iter().any(|v| {
            let v = v.trim();
            if key.ends_with("meta") {
                meta_is_true(meta, v)
            } else {
                format_is(v, for_pandoc)
            }
        });
        if hit == invert {
            return false;
        }
    }
    true
}

/// The source lines with the conditional divs resolved, or none when there
/// are none. Line numbers are kept: what goes becomes a blank line.
pub(crate) fn resolve(lines: &[String], for_pandoc: bool) -> Option<Vec<String>> {
    if !lines
        .iter()
        .any(|l| l.contains("content-visible") || l.contains("content-hidden"))
    {
        return None;
    }
    let texts: Vec<&str> = lines.iter().map(String::as_str).collect();
    let parsed = super::parse(&texts);
    let mut out = lines.to_vec();
    let mut changed = false;
    for div in &parsed.divs {
        let Some(shown) = visibility(&div.classes, &div.attrs, &parsed.fm.meta, for_pandoc) else {
            continue;
        };
        changed = true;
        if shown {
            out[div.start].clear();
            if let Some(end) = div.end {
                out[end].clear();
            }
        } else {
            let last = div.end.unwrap_or(lines.len() - 1);
            for line in &mut out[div.start..=last] {
                line.clear();
            }
        }
    }
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::compose;

    const FIGS: &str = "![One](a.png){#fig-a}\n\n::: {.content-hidden when-format=\"html\"}\n![Two](b.png){#fig-b}\n:::\n\n![Three](c.png){#fig-c}\n\nSee @fig-c.\n";

    #[test]
    fn a_hidden_figure_takes_no_number() {
        let d = compose(FIGS).markdown;
        assert!(!d.contains("b.png"), "{d}");
        assert!(d.contains("Figure 2.</span> Three"), "{d}");
        assert!(d.contains(">Figure 2</a>"), "{d}");
    }

    #[test]
    fn unless_format_html_hides_and_visible_loses_its_wrapper() {
        let d = compose("::: {.content-visible unless-format=\"html\"}\nPDF only\n:::\n\nAfter\n")
            .markdown;
        assert!(!d.contains("PDF only") && d.contains("After"), "{d}");
        let d =
            compose("::: {.content-visible when-format=\"html\" .keep}\nWeb text\n:::\n").markdown;
        assert!(d.contains("Web text") && !d.contains("<div"), "{d}");
    }

    #[test]
    fn when_meta_uses_a_dotted_key_and_strict_true() {
        let front = "---\nflags:\n  draft: true\n  final: yes please\n---\n\n";
        let shown = |attr: &str| {
            let source = format!("{front}::: {{.content-visible {attr}}}\nBODY\n:::\n");
            compose(&source).markdown.contains("BODY")
        };
        assert!(shown("when-meta=\"flags.draft\""));
        assert!(!shown("when-meta=\"flags.final\""));
        assert!(!shown("when-meta=\"flags.missing\""));
        assert!(shown("unless-meta=\"flags.missing\""));
        assert!(!shown("unless-meta=\"flags.draft\""));
    }

    #[test]
    fn hidden_headings_leave_the_contents_and_spans_follow_the_rule() {
        let source = "---\ntoc: true\n---\n\n# Kept\n\n::: {.content-hidden when-format=\"html\"}\n# Gone\n:::\n\nA [x]{.content-hidden when-format=\"html\"}B [y]{.content-visible when-format=\"html\"}C\n";
        let d = compose(source).markdown;
        assert!(!d.contains("Gone"), "{d}");
        assert!(d.contains("AB yC") || d.contains("A B yC"), "{d}");
    }
}
