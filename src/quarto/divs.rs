//! Fenced divs and bracketed spans.
//!
//! Owner: 44vp. The driver parses `:::` fences into a tree of [`Div`]s (any
//! depth) and asks [`open`] and [`close`] for the raw HTML lines that stand
//! for each fence; the lines between them are processed as ordinary content,
//! so Markdown inside a div renders and nested divs recurse. A div whose id
//! carries a cross-reference prefix is offered to `refs::div_open` first.
//! [`render_span`] is the hook for `[text]{...}`.

use std::ops::Range;

use crate::page::escape;

use super::attrs::{parse_attributes, Attrs};
use super::front_matter::{get, set, Map, Yaml};
use super::Context;

/// A parsed `::: {#id .class key=value}` div.
#[derive(Clone, Debug)]
#[allow(dead_code)] // the fields are the feature module's input
pub(crate) struct Div {
    /// 0-based line of the opening fence.
    pub start: usize,
    /// 0-based line of the closing fence; none while it is missing.
    pub end: Option<usize>,
    /// How many divs enclose this one.
    pub depth: usize,
    /// Index of the enclosing div in the document's list.
    pub parent: Option<usize>,
    /// Indexes of the divs directly inside this one.
    pub children: Vec<usize>,
    pub id: String,
    pub classes: Vec<String>,
    pub attrs: Map,
    /// 0-based lines between the fences (an unclosed div runs to the end).
    pub content: Range<usize>,
}

/// `:::` fences: `Some("")` closes a div, `Some(info)` opens one.
pub(crate) fn div_fence(line: &str) -> Option<&str> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let colons = t.bytes().take_while(|&b| b == b':').count();
    if colons < 3 {
        return None;
    }
    let rest = &t[colons..];
    if rest.trim().is_empty() {
        Some("")
    } else if rest.starts_with(char::is_whitespace) || rest.starts_with('{') {
        Some(rest.trim())
    } else {
        None
    }
}

/// What follows the colons of an opening fence. `::: warning` is the same as
/// `::: {.warning}`.
fn info_attributes(info: &str) -> Attrs {
    if info.starts_with('{') {
        parse_attributes(info)
    } else {
        parse_attributes(&format!("{{.{info}}}"))
    }
}

/// Records an opening fence on `line`, nested in whatever is on the stack.
pub(crate) fn open_div(divs: &mut Vec<Div>, stack: &mut Vec<usize>, line: usize, info: &str) {
    let parsed = info_attributes(info);
    let index = divs.len();
    let parent = stack.last().copied();
    if let Some(p) = parent {
        divs[p].children.push(index);
    }
    divs.push(Div {
        start: line,
        end: None,
        depth: stack.len(),
        parent,
        children: Vec::new(),
        id: parsed.id,
        classes: parsed.classes,
        attrs: parsed.attrs,
        content: line + 1..line + 1,
    });
    stack.push(index);
}

/// Records a closing fence on `line`; a stray one closes nothing.
pub(crate) fn close_div(divs: &mut [Div], stack: &mut Vec<usize>, line: usize) {
    if let Some(top) = stack.pop() {
        divs[top].end = Some(line);
        divs[top].content = divs[top].start + 1..line;
    }
}

/// Gives the divs still open at the end of the file the rest of the file.
pub(crate) fn finish_unclosed(divs: &mut [Div], stack: &[usize], line_count: usize) {
    for &index in stack {
        divs[index].content = divs[index].start + 1..line_count;
    }
}

/// ` class="a b" id="x"`: the generic rule, in that order, nothing for what
/// is empty.
pub(crate) fn html_attributes(id: &str, classes: &[String]) -> String {
    let mut out = String::new();
    if !classes.is_empty() {
        out.push_str(&format!(" class=\"{}\"", escape(&classes.join(" "))));
    }
    if !id.is_empty() {
        out.push_str(&format!(" id=\"{}\"", escape(id)));
    }
    out
}

const CALLOUTS: [&str; 5] = ["note", "tip", "warning", "important", "caution"];

/// Keys a feature consumes, so they are not passed through as attributes.
const CONSUMED: [&str; 4] = ["title", "collapse", "appearance", "icon"];

/// The callout kind of a div, from a `callout-KIND` class.
fn callout_kind(div: &Div) -> Option<&'static str> {
    div.classes.iter().find_map(|c| {
        let kind = c.strip_prefix("callout-")?;
        CALLOUTS.iter().copied().find(|k| *k == kind)
    })
}

fn has_class(div: &Div, name: &str) -> bool {
    div.classes.iter().any(|c| c == name)
}

fn is_margin(div: &Div) -> bool {
    has_class(div, "column-margin") || has_class(div, "aside")
}

fn is_tabset(div: &Div) -> bool {
    has_class(div, "panel-tabset")
}

fn attr_text(map: &Map, key: &str) -> Option<String> {
    get(map, key).map(|v| v.text())
}

/// ` key="value"` for every attribute that is not consumed by a feature.
/// Event handlers and keys that cannot be attribute names are dropped.
fn extra_attributes(map: &Map, skip: &[&str]) -> String {
    let mut out = String::new();
    for (key, value) in map {
        let valid = !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
        let lower = key.to_ascii_lowercase();
        if !valid || lower.starts_with("on") || skip.contains(&key.as_str()) {
            continue;
        }
        out.push_str(&format!(" {key}=\"{}\"", escape(&value.text())));
    }
    out
}

/// ` class=".." id=".." ...` for a tag.
fn tag_attributes(id: &str, classes: &[String], map: &Map, skip: &[&str]) -> String {
    format!(
        "{}{}",
        html_attributes(id, classes),
        extra_attributes(map, skip)
    )
}

/// `(level, text)` of an ATX heading line; a trailing `{...}` is dropped.
fn atx_heading(line: &str) -> Option<(usize, String)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let level = t.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &t[level..];
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let mut text = rest.trim();
    if text.ends_with('}') {
        if let Some(at) = text.rfind('{') {
            text = text[..at].trim_end();
        }
    }
    let text = text.trim_end_matches('#').trim_end();
    Some((level, text.to_string()))
}

fn fence_marker(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// The headings directly inside a div (not in nested divs or code fences):
/// `(line, level, text)`.
fn direct_headings(div: &Div, lines: &[&str]) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut fenced = false;
    for i in div.content.clone() {
        let Some(line) = lines.get(i) else { break };
        if fence_marker(line) {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        match div_fence(line) {
            Some("") => depth = depth.saturating_sub(1),
            Some(_) => depth += 1,
            None => {
                if depth == 0 {
                    if let Some((level, text)) = atx_heading(line) {
                        out.push((i, level, text));
                    }
                }
            }
        }
    }
    out
}

/// A callout's leading heading: the first non-blank line, when a heading.
fn leading_heading(div: &Div, lines: &[&str]) -> Option<(usize, String)> {
    let i = div
        .content
        .clone()
        .find(|&i| lines.get(i).is_some_and(|l| !l.trim().is_empty()))?;
    atx_heading(lines[i]).map(|(_, text)| (i, text))
}

/// The headings that start the tabs: those at the level of the first one.
fn tab_headings(div: &Div, lines: &[&str]) -> Vec<(usize, String)> {
    let all = direct_headings(div, lines);
    let Some(&(_, level, _)) = all.first() else {
        return Vec::new();
    };
    all.into_iter()
        .filter(|h| h.1 == level)
        .map(|h| (h.0, h.2))
        .collect()
}

/// Whether the first non-blank line of a tabset is a tab heading.
fn starts_with_tab(div: &Div, lines: &[&str]) -> bool {
    let first = div
        .content
        .clone()
        .find(|&i| lines.get(i).is_some_and(|l| !l.trim().is_empty()));
    match (first, tab_headings(div, lines).first()) {
        (Some(a), Some(&(b, _))) => a == b,
        _ => false,
    }
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn callout_open(div: &Div, kind: &str, lines: &[&str]) -> String {
    let title = attr_text(&div.attrs, "title")
        .or_else(|| leading_heading(div, lines).map(|h| h.1))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| capitalised(kind));
    let mut classes = vec!["callout".to_string(), format!("callout-{kind}")];
    match attr_text(&div.attrs, "appearance").as_deref() {
        Some("simple") => classes.push("callout-simple".into()),
        Some("minimal") => classes.push("callout-minimal".into()),
        _ => {}
    }
    classes.extend(
        div.classes
            .iter()
            .filter(|c| c.strip_prefix("callout-") != Some(kind))
            .cloned(),
    );
    let attrs = tag_attributes(&div.id, &classes, &div.attrs, &CONSUMED);
    let title = escape(&title);
    match attr_text(&div.attrs, "collapse").as_deref() {
        Some(c @ ("true" | "false")) => {
            let open = if c == "false" { " open" } else { "" };
            format!(
                "<details{attrs}{open}><summary class=\"callout-title\">{title}</summary><div class=\"callout-body\">"
            )
        }
        _ => format!(
            "<div{attrs}><div class=\"callout-title\">{title}</div><div class=\"callout-body\">"
        ),
    }
}

/// The raw HTML line that opens a div, or none to emit nothing. `lines` are
/// the source lines of the document, for the features that read the div's
/// content (a callout's heading, the tabs of a tabset).
pub(crate) fn open(div: &Div, lines: &[&str], _ctx: &mut Context) -> Option<String> {
    if let Some(kind) = callout_kind(div) {
        return Some(callout_open(div, kind, lines));
    }
    if is_margin(div) {
        let classes: Vec<String> = std::iter::once("margin".to_string())
            .chain(
                div.classes
                    .iter()
                    .filter(|c| !matches!(c.as_str(), "aside" | "column-margin"))
                    .cloned(),
            )
            .collect();
        return Some(format!(
            "<aside{}>",
            tag_attributes(&div.id, &classes, &div.attrs, &CONSUMED)
        ));
    }
    if is_tabset(div) {
        let classes: Vec<String> = std::iter::once("tabset".to_string())
            .chain(div.classes.iter().filter(|c| *c != "panel-tabset").cloned())
            .collect();
        let tag = format!(
            "<div{}>",
            tag_attributes(&div.id, &classes, &div.attrs, &CONSUMED)
        );
        // The first tab is opened here unless a heading opens it.
        return Some(if starts_with_tab(div, lines) {
            tag
        } else {
            format!("{tag}<div class=\"tab\">")
        });
    }
    let mut attrs = div.attrs.clone();
    let mut skip: Vec<&str> = CONSUMED.to_vec();
    if has_class(div, "column") {
        skip.push("width");
        if let Some(width) = attr_text(&attrs, "width") {
            let basis = format!("flex-basis:{width}");
            let style = match attr_text(&attrs, "style") {
                Some(s) if !s.trim().is_empty() => format!("{basis};{s}"),
                _ => basis,
            };
            set(&mut attrs, "style", Yaml::Str(style));
        }
    }
    Some(format!(
        "<div{}>",
        tag_attributes(&div.id, &div.classes, &attrs, &skip)
    ))
}

/// The raw HTML line that closes a div opened by [`open`].
pub(crate) fn close(div: &Div, _ctx: &mut Context) -> Option<String> {
    Some(
        if callout_kind(div).is_some() {
            if matches!(
                attr_text(&div.attrs, "collapse").as_deref(),
                Some("true" | "false")
            ) {
                "</div></details>"
            } else {
                "</div></div>"
            }
        } else if is_margin(div) {
            "</aside>"
        } else if is_tabset(div) {
            "</div></div>"
        } else {
            "</div>"
        }
        .to_string(),
    )
}

/// Replaces a content line of `div` (the innermost open div), or none to
/// leave it as ordinary content: the heading that titles a callout becomes
/// nothing (an empty string), and a tab heading starts a tab.
pub(crate) fn content_line(
    div: &Div,
    lines: &[&str],
    line: usize,
    _ctx: &mut Context,
) -> Option<String> {
    if callout_kind(div).is_some() {
        if get(&div.attrs, "title").is_none()
            && leading_heading(div, lines).is_some_and(|h| h.0 == line)
        {
            return Some(String::new());
        }
        return None;
    }
    if is_tabset(div) {
        let tabs = tab_headings(div, lines);
        let at = tabs.iter().position(|t| t.0 == line)?;
        let close = if at == 0 && starts_with_tab(div, lines) {
            ""
        } else {
            "</div>"
        };
        return Some(format!(
            "{close}<div class=\"tab\"><p class=\"tab-title\">{}</p>",
            escape(&tabs[at].1)
        ));
    }
    None
}

/// The HTML of a bracketed span `[content]{...}`.
pub(crate) fn render_span(content: &str, attrs: &Attrs, _ctx: &mut Context) -> String {
    let has = |name: &str| attrs.classes.iter().any(|c| c == name);
    let (tag, classes): (&str, Vec<String>) = if has("mark") {
        ("mark", attrs.classes.iter().filter(|c| *c != "mark").cloned().collect())
    } else {
        (
            "span",
            attrs
                .classes
                .iter()
                .map(|c| if c == "aside" { "margin".to_string() } else { c.clone() })
                .collect(),
        )
    };
    format!(
        "<{tag}{}>{content}</{tag}>",
        tag_attributes(&attrs.id, &classes, &attrs.attrs, &CONSUMED)
    )
}

/// The end of the `{...}` block that starts at `from`, quotes respected.
fn attribute_block_end(text: &str, from: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (at, c) in text[from..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '}') => return Some(from + at),
            _ => {}
        }
    }
    None
}

/// `[text]{...}` becomes a span, through [`render_span`].
pub(crate) fn spans(text: &str, ctx: &mut Context) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while let Some(at) = text[i..].find('[') {
        let start = i + at;
        i = start + 1;
        let Some(close) = text[start + 1..].find(']') else {
            break;
        };
        let close = start + 1 + close;
        if close == start + 1 || !text[close + 1..].starts_with('{') {
            continue;
        }
        let Some(brace) = attribute_block_end(text, close + 2) else {
            continue;
        };
        let attrs = parse_attributes(&text[close + 1..=brace]);
        let mut id_chars = attrs.id.chars();
        let valid_id = attrs.id.is_empty()
            || (id_chars.next().is_some_and(|c| c.is_ascii_alphabetic())
                && id_chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-')));
        if !valid_id || (attrs.id.is_empty() && attrs.classes.is_empty() && attrs.attrs.is_empty())
        {
            continue;
        }
        out.push_str(&text[copied..start]);
        out.push_str(&render_span(&text[start + 1..close], &attrs, ctx));
        copied = brace + 1;
        i = copied;
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::compose;

    #[test]
    fn a_div_is_rendered_around_its_markdown() {
        assert_eq!(
            compose(":::{.box}\nCompact note.\n:::\n").markdown,
            "\n<div class=\"box\">\n\nCompact note.\n\n</div>\n\n"
        );
        let d = compose("::: {.box}\n<div>\n  first\n</div>\n:::\n").markdown;
        assert!(
            d.starts_with("\n<div class=\"box\">\n\n<div>\n  first\n</div>\n\n</div>\n"),
            "{d}"
        );
        assert_eq!(
            compose("::: {.box}\nStill typing\n  indented").markdown,
            "\n<div class=\"box\">\n\nStill typing\n  indented\n\n</div>\n"
        );
    }

    #[test]
    fn divs_nest_and_the_generated_lines_belong_to_the_outer_fence() {
        let d = compose("::: {.a}\nOuter\n::: {#in .b .c}\nInner *text*\n:::\n:::\n");
        assert!(d.markdown.contains("<div class=\"a\">"), "{}", d.markdown);
        assert!(
            d.markdown.contains("<div class=\"b c\" id=\"in\">"),
            "{}",
            d.markdown
        );
        assert_eq!(d.markdown.matches("</div>").count(), 2);
        assert!(!d.markdown.contains("```"), "{}", d.markdown);
        // The opening line of the outer div claims the whole div.
        let at = d
            .markdown
            .split('\n')
            .position(|l| l == "<div class=\"a\">")
            .unwrap();
        assert_eq!((d.line_map[at], d.origin[at]), (None, (1, 6)));
        let inner = d
            .markdown
            .split('\n')
            .position(|l| l == "<div class=\"b c\" id=\"in\">")
            .unwrap();
        assert_eq!(d.origin[inner], (3, 5));
        let text = d
            .markdown
            .split('\n')
            .position(|l| l == "Inner *text*")
            .unwrap();
        assert_eq!(d.line_map[text], Some(4));
    }

    #[test]
    fn a_bare_word_is_a_class_and_values_are_escaped() {
        let d = compose("::: warning\nx\n:::\n\n::: {#a&b}\ny\n:::\n").markdown;
        assert!(d.contains("<div class=\"warning\">"), "{d}");
        assert!(d.contains("<div id=\"a&amp;b\">"), "{d}");
    }

    #[test]
    fn a_stray_closing_fence_stays_text() {
        assert!(compose("text\n:::\n").markdown.contains("\n:::\n"));
    }

    #[test]
    fn a_callout_has_a_title_and_a_body() {
        let d = compose("::: {.callout-note}\nBody *text*.\n:::\n").markdown;
        assert!(
            d.contains("<div class=\"callout callout-note\"><div class=\"callout-title\">Note</div><div class=\"callout-body\">"),
            "{d}"
        );
        assert!(d.contains("Body *text*.") && d.contains("</div></div>"), "{d}");
        let d = compose("::: {.callout-tip title=\"Fast & easy\"}\nx\n:::\n").markdown;
        assert!(d.contains("<div class=\"callout-title\">Fast &amp; easy</div>"), "{d}");
    }

    #[test]
    fn a_leading_heading_titles_the_callout() {
        let d = compose("::: {.callout-warning}\n## Mind the gap\n\nBody.\n:::\n").markdown;
        assert!(d.contains("<div class=\"callout-title\">Mind the gap</div>"), "{d}");
        assert!(!d.contains("## Mind the gap"), "{d}");
        assert!(d.contains("Body."), "{d}");
        // An explicit title keeps the heading in the body.
        let d = compose("::: {.callout-warning title=\"T\"}\n## H\n:::\n").markdown;
        assert!(d.contains("## H") && d.contains(">T</div>"), "{d}");
    }

    #[test]
    fn a_collapsible_callout_is_details() {
        let d = compose("::: {.callout-tip collapse=\"true\"}\nx\n:::\n").markdown;
        assert!(d.contains("<details class=\"callout callout-tip\"><summary class=\"callout-title\">Tip</summary><div class=\"callout-body\">"), "{d}");
        assert!(d.contains("</div></details>"), "{d}");
        let d = compose("::: {.callout-tip collapse=false}\nx\n:::\n").markdown;
        assert!(d.contains("<details class=\"callout callout-tip\" open>"), "{d}");
    }

    #[test]
    fn callout_appearance_adds_a_class() {
        let d = compose("::: {.callout-caution appearance=\"simple\"}\nx\n:::\n").markdown;
        assert!(d.contains("class=\"callout callout-caution callout-simple\""), "{d}");
        let d = compose("::: {.callout-important appearance=minimal icon=false}\nx\n:::\n").markdown;
        assert!(d.contains("class=\"callout callout-important callout-minimal\""), "{d}");
        assert!(!d.contains("icon"), "{d}");
    }

    #[test]
    fn a_generic_div_passes_attributes_through() {
        let d = compose("::: {#b .wide style=\"color:red\" data-x=\"a\\\"b\" onclick=bad}\nx\n:::\n").markdown;
        assert!(d.contains("<div class=\"wide\" id=\"b\" style=\"color:red\" data-x=\"a&quot;b\">"), "{d}");
        assert!(!d.contains("onclick"), "{d}");
    }

    #[test]
    fn columns_carry_their_width() {
        let d = compose("::: columns\n::: {.column width=\"40%\"}\nA\n:::\n::: {.column}\nB\n:::\n:::\n").markdown;
        assert!(d.contains("<div class=\"columns\">"), "{d}");
        assert!(d.contains("<div class=\"column\" style=\"flex-basis:40%\">"), "{d}");
        assert!(d.contains("<div class=\"column\">"), "{d}");
    }

    #[test]
    fn margin_content_is_an_aside() {
        let d = compose("::: {.column-margin}\nNote\n:::\n\n::: aside\nMore\n:::\n").markdown;
        assert_eq!(d.matches("<aside class=\"margin\">").count(), 2, "{d}");
        assert_eq!(d.matches("</aside>").count(), 2, "{d}");
        let d = compose("Text [beside]{.aside} text.\n").markdown;
        assert!(d.contains("<span class=\"margin\">beside</span>"), "{d}");
    }

    #[test]
    fn a_tabset_starts_a_tab_at_each_heading() {
        let d = compose("::: {.panel-tabset}\n## R\n\nr code\n\n## Python\n\npy code\n:::\n").markdown;
        assert!(d.contains("<div class=\"tabset\">"), "{d}");
        assert!(d.contains("<div class=\"tab\"><p class=\"tab-title\">R</p>"), "{d}");
        assert!(d.contains("</div><div class=\"tab\"><p class=\"tab-title\">Python</p>"), "{d}");
        assert!(d.contains("</div></div>"), "{d}");
        // The divs balance: tabset, two tabs.
        assert_eq!(d.matches("<div").count(), 3, "{d}");
        assert_eq!(d.matches("</div>").count(), 3, "{d}");
    }

    #[test]
    fn spans_take_classes_and_attributes() {
        let d = compose("[Small]{.smallcaps} [under]{.underline} [hi]{.mark} [x]{.a #i title=\"t\"}\n").markdown;
        assert!(d.contains("<span class=\"smallcaps\">Small</span>"), "{d}");
        assert!(d.contains("<span class=\"underline\">under</span>"), "{d}");
        assert!(d.contains("<mark>hi</mark>"), "{d}");
        assert!(d.contains("<span class=\"a\" id=\"i\" title=\"t\">x</span>"), "{d}");
        // A link is not a span.
        assert!(compose("[t](u){.x}\n").markdown.contains("[t](u){.x}"));
    }
}

#[cfg(test)]
mod block_tests {
    use crate::blocks::compile_blocks;
    use crate::markdown;

    #[test]
    fn a_callout_folds_into_one_block() {
        let no_files = |_: &str| None;
        let blocks = compile_blocks(
            "::: {.callout-note}\n## Heads up\n\nFirst.\n\nSecond.\n:::\n",
            "paper.qmd",
            &no_files,
            &markdown::no_assets,
        );
        let value: serde_json::Value = serde_json::from_str(&blocks.json).unwrap();
        let list = value["blocks"].as_array().unwrap();
        assert_eq!(list.len(), 1, "{}", blocks.json);
        let html = list[0]["html"].as_str().unwrap();
        assert!(html.contains("callout-title\">Heads up</div>"), "{html}");
        assert!(html.contains("<p>First.</p>") && html.contains("<p>Second.</p>"), "{html}");
    }
}
