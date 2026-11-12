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
use super::front_matter::Map;
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

/// The raw HTML line that opens a div, or none to emit nothing. The default
/// is `<div id=... class=...>`; the extension point for per-class markup.
pub(crate) fn open(div: &Div, _ctx: &mut Context) -> Option<String> {
    Some(format!("<div{}>", html_attributes(&div.id, &div.classes)))
}

/// The raw HTML line that closes a div opened by [`open`].
pub(crate) fn close(_div: &Div, _ctx: &mut Context) -> Option<String> {
    Some("</div>".to_string())
}

/// The HTML of a bracketed span `[content]{...}`. The extension point for
/// span classes and attributes; today only `{#id}` is recognised.
pub(crate) fn render_span(content: &str, attrs: &Attrs, _ctx: &mut Context) -> String {
    format!("<span id=\"{}\">{content}</span>", escape(&attrs.id))
}

/// `[text]{#id}` becomes a span, through [`render_span`].
pub(crate) fn spans(text: &str, ctx: &mut Context) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while let Some(at) = text[i..].find('[') {
        let start = i + at;
        i = start + 1;
        let Some(close) = text[start + 1..].find(']') else { break };
        let close = start + 1 + close;
        if close == start + 1 || !text[close + 1..].starts_with("{#") {
            continue;
        }
        let id_start = close + 3;
        let Some(brace) = text[id_start..].find('}') else { continue };
        let id = &text[id_start..id_start + brace];
        let mut chars = id.chars();
        let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'));
        if !valid {
            continue;
        }
        out.push_str(&text[copied..start]);
        let attrs = Attrs { id: id.to_string(), ..Attrs::default() };
        out.push_str(&render_span(&text[start + 1..close], &attrs, ctx));
        copied = id_start + brace + 1;
        i = copied;
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quarto::tests::compose;

    #[test]
    fn a_div_is_rendered_around_its_markdown() {
        assert_eq!(
            compose(":::{.callout-note}\nCompact note.\n:::\n").markdown,
            "\n<div class=\"callout-note\">\n\nCompact note.\n\n</div>\n\n"
        );
        let d = compose("::: {.callout-note}\n<div>\n  first\n</div>\n:::\n").markdown;
        assert!(d.starts_with("\n<div class=\"callout-note\">\n\n<div>\n  first\n</div>\n\n</div>\n"), "{d}");
        assert_eq!(
            compose("::: {.callout-note}\nStill typing\n  indented").markdown,
            "\n<div class=\"callout-note\">\n\nStill typing\n  indented\n\n</div>\n"
        );
    }

    #[test]
    fn divs_nest_and_the_generated_lines_belong_to_the_outer_fence() {
        let d = compose("::: {.a}\nOuter\n::: {#in .b .c}\nInner *text*\n:::\n:::\n");
        assert!(d.markdown.contains("<div class=\"a\">"), "{}", d.markdown);
        assert!(d.markdown.contains("<div class=\"b c\" id=\"in\">"), "{}", d.markdown);
        assert_eq!(d.markdown.matches("</div>").count(), 2);
        assert!(!d.markdown.contains("```"), "{}", d.markdown);
        // The opening line of the outer div claims the whole div.
        let at = d.markdown.split('\n').position(|l| l == "<div class=\"a\">").unwrap();
        assert_eq!((d.line_map[at], d.origin[at]), (None, (1, 6)));
        let inner = d.markdown.split('\n').position(|l| l == "<div class=\"b c\" id=\"in\">").unwrap();
        assert_eq!(d.origin[inner], (3, 5));
        let text = d.markdown.split('\n').position(|l| l == "Inner *text*").unwrap();
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
}
