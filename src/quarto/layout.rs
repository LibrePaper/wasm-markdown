//! Caption location, wide-column classes and layout alignment and rows.
//!
//! The pieces of Quarto's page layout that are decisions about classes and
//! widths rather than about numbering: where a float's caption goes
//! (`fig-cap-location`, `tbl-cap-location`, `cap-location`), which wide
//! column class a float or a cell output wears (`fig-column`, `tbl-column`,
//! `column`, and the `.column-*` classes on a div), the alignment classes of a
//! layout (`layout-align`, `layout-valign`) and the rows of a `layout="[...]"`
//! with negative numbers as spacers. The callers in `refs.rs`, `outputs.rs`
//! and `cells.rs` write the markup; this file only answers.

use super::front_matter::{get, Map, Yaml};

/// Where a float's caption goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CapLocation {
    Top,
    #[default]
    Bottom,
    Margin,
}

impl CapLocation {
    fn parse(text: &str) -> Option<CapLocation> {
        match text.trim() {
            "top" => Some(CapLocation::Top),
            "bottom" => Some(CapLocation::Bottom),
            "margin" => Some(CapLocation::Margin),
            _ => None,
        }
    }
}

/// The text of an option, none when it is absent, null or blank.
fn option_text(map: &Map, key: &str) -> Option<String> {
    get(map, key)
        .filter(|v| **v != Yaml::Null)
        .map(Yaml::text)
        .filter(|t| !t.trim().is_empty())
}

/// A document option: `format: html:` first, then the top level.
fn doc_option(meta: &Map, key: &str) -> Option<String> {
    if let Some(Yaml::Map(format)) = get(meta, "format") {
        if let Some(Yaml::Map(html)) = get(format, "html") {
            if let Some(text) = option_text(html, key) {
                return Some(text);
            }
        }
    }
    option_text(meta, key)
}

/// Where the caption of a float of `kind` (`fig`, `tbl`) goes: the element's
/// `{kind}-cap-location` or `cap-location`, then the document's, then
/// Quarto's default (figures below, tables above).
pub(crate) fn cap_location(element: &Map, meta: &Map, kind: &str) -> CapLocation {
    let qualified = format!("{kind}-cap-location");
    let keys = [qualified.as_str(), "cap-location"];
    keys.iter()
        .find_map(|k| option_text(element, k).and_then(|t| CapLocation::parse(&t)))
        .or_else(|| {
            keys.iter()
                .find_map(|k| doc_option(meta, k).and_then(|t| CapLocation::parse(&t)))
        })
        .unwrap_or(if kind == "tbl" {
            CapLocation::Top
        } else {
            CapLocation::Bottom
        })
}

/// `page` -> `column-page`; `margin` -> `margin`, the class margin content
/// wears here. None when nothing usable is left.
pub(crate) fn column_class_of(value: &str) -> Option<String> {
    let v: String = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    match v.as_str() {
        "" => None,
        "margin" => Some("margin".to_string()),
        _ => Some(format!("column-{v}")),
    }
}

/// The column class of a float of `kind`: the element's `{kind}-column`, then
/// the document's.
pub(crate) fn column_class(element: &Map, meta: &Map, kind: &str) -> Option<String> {
    let key = format!("{kind}-column");
    option_text(element, &key)
        .or_else(|| doc_option(meta, &key))
        .and_then(|v| column_class_of(&v))
}

/// The wide-column classes among a div's classes, kept as written;
/// `.column-margin` and `.aside` become `margin`.
pub(crate) fn div_columns(classes: &[String]) -> Vec<String> {
    classes
        .iter()
        .filter_map(|c| match c.as_str() {
            "column-margin" | "aside" => Some("margin".to_string()),
            other if other.starts_with("column-") => Some(other.to_string()),
            _ => None,
        })
        .collect()
}

/// `quarto-layout-align-center` and `quarto-layout-valign-top` for the
/// `layout-align` and `layout-valign` of a layout.
pub(crate) fn align_classes(attrs: &Map) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(a) = option_text(attrs, "layout-align") {
        if matches!(a.trim(), "left" | "center" | "right") {
            out.push(format!("quarto-layout-align-{}", a.trim()));
        }
    }
    if let Some(v) = option_text(attrs, "layout-valign") {
        if matches!(v.trim(), "top" | "center" | "bottom") {
            out.push(format!("quarto-layout-valign-{}", v.trim()));
        }
    }
    out
}

/// What an attribute value says as text, with a bracketed list written back
/// the way it was (the attribute reader splits `[[1,1],[1]]` at its commas).
pub(crate) fn flat_text(value: &Yaml) -> String {
    match value {
        Yaml::List(items) => format!(
            "[{}]",
            items.iter().map(flat_text).collect::<Vec<_>>().join(",")
        ),
        other => other.text(),
    }
}

/// One cell of a layout: the markup that goes before it (a row break, the
/// spacers that precede it) and its width.
#[derive(Clone, Debug, Default)]
pub(crate) struct CellPlan {
    pub before: String,
    /// `33.33%`.
    pub width: String,
}

const BREAK: &str = "<div class=\"quarto-layout-break\"></div>";

/// The rows of `[[1,1],[1]]` (or one row, `[1,1]`); a negative number is a
/// spacer.
fn parse_rows(spec: &str) -> Vec<Vec<f64>> {
    let mut rows: Vec<Vec<f64>> = Vec::new();
    let mut flat: Vec<f64> = Vec::new();
    let mut current: Option<Vec<f64>> = None;
    let mut depth = 0usize;
    let mut num = String::new();
    let flush = |num: &mut String, current: &mut Option<Vec<f64>>, flat: &mut Vec<f64>| {
        if let Ok(n) = num.parse::<f64>() {
            match current {
                Some(row) => row.push(n),
                None => flat.push(n),
            }
        }
        num.clear();
    };
    for c in spec.chars() {
        match c {
            '[' => {
                depth += 1;
                if depth == 2 {
                    current = Some(Vec::new());
                }
            }
            ']' => {
                flush(&mut num, &mut current, &mut flat);
                if depth == 2 {
                    rows.extend(current.take());
                }
                depth = depth.saturating_sub(1);
            }
            ',' => flush(&mut num, &mut current, &mut flat),
            c if c.is_ascii_digit() || matches!(c, '.' | '-' | '+') => num.push(c),
            _ => {}
        }
    }
    flush(&mut num, &mut current, &mut flat);
    if rows.is_empty() && !flat.is_empty() {
        rows.push(flat);
    }
    rows
}

fn percent(part: f64, total: f64) -> String {
    let text = format!("{:.2}", part / total * 100.0);
    format!("{}%", text.trim_end_matches('0').trim_end_matches('.'))
}

/// Lays `cells` cells out in the rows of `spec`. Each row is a share of the
/// width (a negative number is empty space of that share); when the rows run
/// out the last one repeats, and when the cells run out the rest is dropped.
pub(crate) fn plan(spec: &str, cells: usize) -> Vec<CellPlan> {
    let rows = parse_rows(spec);
    let mut plans: Vec<CellPlan> = Vec::new();
    if rows.is_empty() || cells == 0 {
        return plans;
    }
    let mut pending = String::new();
    let mut started = false;
    let mut r = 0;
    while plans.len() < cells {
        let row = &rows[r.min(rows.len() - 1)];
        if r >= rows.len() && !row.iter().any(|w| *w > 0.0) {
            break;
        }
        r += 1;
        let total: f64 = row.iter().map(|w| w.abs()).sum();
        if total <= 0.0 {
            continue;
        }
        if started {
            pending.push_str(BREAK);
        }
        started = true;
        for w in row {
            if plans.len() >= cells {
                break;
            }
            let width = percent(w.abs(), total);
            if *w < 0.0 {
                pending.push_str(&format!(
                    "<div class=\"quarto-figure-spacer\" style=\"flex-basis:{width}\"></div>"
                ));
            } else if *w > 0.0 {
                plans.push(CellPlan {
                    before: std::mem::take(&mut pending),
                    width,
                });
            }
        }
    }
    plans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> Map {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Yaml::Str(v.to_string())))
            .collect()
    }

    #[test]
    fn caption_location_defaults_and_precedence() {
        let none = Map::new();
        assert_eq!(cap_location(&none, &none, "fig"), CapLocation::Bottom);
        assert_eq!(cap_location(&none, &none, "tbl"), CapLocation::Top);
        let meta = map(&[("fig-cap-location", "top")]);
        assert_eq!(cap_location(&none, &meta, "fig"), CapLocation::Top);
        let element = map(&[("cap-location", "margin")]);
        assert_eq!(cap_location(&element, &meta, "fig"), CapLocation::Margin);
        let junk = map(&[("cap-location", "sideways")]);
        assert_eq!(cap_location(&junk, &none, "fig"), CapLocation::Bottom);
    }

    #[test]
    fn column_classes() {
        assert_eq!(
            column_class_of("page-right").as_deref(),
            Some("column-page-right")
        );
        assert_eq!(column_class_of("margin").as_deref(), Some("margin"));
        assert_eq!(column_class_of(" "), None);
        let classes = vec!["column-page".to_string(), "x".into(), "aside".into()];
        assert_eq!(div_columns(&classes), ["column-page", "margin"]);
    }

    #[test]
    fn rows_and_spacers() {
        let plans = plan("[[1,-1,1],[1]]", 3);
        assert_eq!(plans.len(), 3);
        assert_eq!(plans[0].width, "33.33%");
        assert!(
            plans[1].before.contains("quarto-figure-spacer"),
            "{:?}",
            plans[1]
        );
        assert_eq!(plans[2].before, BREAK);
        assert_eq!(plans[2].width, "100%");
        // The last row repeats while cells remain.
        let plans = plan("[[1,1]]", 3);
        assert_eq!(plans[2].before, BREAK);
        assert_eq!(plans[2].width, "50%");
        assert_eq!(parse_rows("[1,2]"), vec![vec![1.0, 2.0]]);
    }
}
