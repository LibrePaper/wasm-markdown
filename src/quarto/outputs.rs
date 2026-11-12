//! Saved cell outputs in the house vocabulary.
//!
//! With Run code off the host passes the HTML Quarto produced for a cell in an
//! earlier real render. [`normalize`] reads it with a small tag scanner and
//! writes it again without Quarto's and Bootstrap's scaffolding: the
//! `div.cell` and float wrappers go, a float becomes `figure.figure` or
//! `figure.table` whose caption label comes from our own numbering (so it
//! matches the `@fig-x` references of the draft), the Bootstrap classes go,
//! and `cell-output-*` blocks stay. Anything unknown passes through, and
//! HTML that does not scan comes back as it was.

use super::cells::Cell;
use super::Context;
use crate::page::escape;

enum Node {
    Text(String),
    /// A comment or declaration, kept as written.
    Raw(String),
    Elem(Elem),
}

struct Elem {
    name: String,
    attrs: Vec<(String, Option<String>)>,
    raw_open: String,
    children: Vec<Node>,
    /// The closing tag as written, when there was one.
    close: Option<String>,
}

impl Elem {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_deref().unwrap_or(""))
    }
    fn has_class(&self, class: &str) -> bool {
        self.attr("class")
            .is_some_and(|c| c.split_whitespace().any(|t| t == class))
    }
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];
/// Elements HTML lets a closing tag or the end of the input finish.
const IMPLICIT: &[&str] = &[
    "p", "li", "td", "th", "tr", "dt", "dd", "thead", "tbody", "tfoot", "option",
];

// ------------------------------------------------------------------ scanner

fn parse(html: &str) -> Option<Vec<Node>> {
    let b = html.as_bytes();
    let mut stack: Vec<Elem> = Vec::new();
    let mut top: Vec<Node> = Vec::new();
    let mut i = 0;
    macro_rules! push {
        ($node:expr) => {
            match stack.last_mut() {
                Some(e) => e.children.push($node),
                None => top.push($node),
            }
        };
    }
    while i < b.len() {
        if b[i] != b'<' {
            let end = html[i..].find('<').map_or(b.len(), |p| i + p);
            push!(Node::Text(html[i..end].to_string()));
            i = end;
            continue;
        }
        let next = b.get(i + 1).copied().unwrap_or(0);
        if html[i..].starts_with("<!--") {
            let end = i + html[i..].find("-->")? + 3;
            push!(Node::Raw(html[i..end].to_string()));
            i = end;
        } else if next == b'!' || next == b'?' {
            let end = i + html[i..].find('>')? + 1;
            push!(Node::Raw(html[i..end].to_string()));
            i = end;
        } else if next == b'/' {
            let end = i + html[i..].find('>')? + 1;
            let name = html[i + 2..end - 1].trim().to_ascii_lowercase();
            // Finish implicitly closed elements on the way to the match.
            let at = stack.iter().rposition(|e| e.name == name)?;
            if stack[at + 1..]
                .iter()
                .any(|e| !IMPLICIT.contains(&e.name.as_str()))
            {
                return None;
            }
            while stack.len() > at + 1 {
                let done = stack.pop()?;
                push!(Node::Elem(done));
            }
            let mut done = stack.pop()?;
            done.close = Some(html[i..end].to_string());
            push!(Node::Elem(done));
            i = end;
        } else if next.is_ascii_alphabetic() {
            let (mut elem, end, self_closing) = open_tag(html, i)?;
            i = end;
            if matches!(elem.name.as_str(), "script" | "style") && !self_closing {
                let close = format!("</{}", elem.name);
                let lower = html[i..].to_ascii_lowercase();
                let at = i + lower.find(&close)?;
                let end = at + html[at..].find('>')? + 1;
                if at > i {
                    elem.children.push(Node::Text(html[i..at].to_string()));
                }
                elem.close = Some(html[at..end].to_string());
                push!(Node::Elem(elem));
                i = end;
            } else if self_closing || VOID.contains(&elem.name.as_str()) {
                push!(Node::Elem(elem));
            } else {
                stack.push(elem);
            }
        } else {
            push!(Node::Text("<".to_string()));
            i += 1;
        }
    }
    while let Some(done) = stack.pop() {
        if !IMPLICIT.contains(&done.name.as_str()) {
            return None;
        }
        push!(Node::Elem(done));
    }
    Some(top)
}

/// The start tag at `at`, where it ends, and whether it ended in `/>`.
fn open_tag(html: &str, at: usize) -> Option<(Elem, usize, bool)> {
    let b = html.as_bytes();
    let mut i = at + 1;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b':' || b[i] == b'-') {
        i += 1;
    }
    let name = html[at + 1..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        match *b.get(i)? {
            b'>' => {
                i += 1;
                break;
            }
            b'/' if b.get(i + 1) == Some(&b'>') => {
                let raw_open = html[at..i + 2].to_string();
                return Some((
                    Elem {
                        name,
                        attrs,
                        raw_open,
                        children: Vec::new(),
                        close: None,
                    },
                    i + 2,
                    true,
                ));
            }
            b'/' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        if i == start {
            return None;
        }
        let key = html[start..i].to_ascii_lowercase();
        let mut j = i;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if b.get(j) == Some(&b'=') {
            j += 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let value = match *b.get(j)? {
                q @ (b'"' | b'\'') => {
                    let close = j + 1 + html[j + 1..].find(q as char)?;
                    let v = html[j + 1..close].to_string();
                    i = close + 1;
                    v
                }
                _ => {
                    let s = j;
                    while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                        j += 1;
                    }
                    i = j;
                    html[s..j].to_string()
                }
            };
            attrs.push((key, Some(value)));
        } else {
            attrs.push((key, None));
        }
    }
    let raw_open = html[at..i].to_string();
    Some((
        Elem {
            name,
            attrs,
            raw_open,
            children: Vec::new(),
            close: None,
        },
        i,
        false,
    ))
}

// ----------------------------------------------------------------- the writer

struct State<'a, 'b> {
    ctx: &'a Context<'b>,
    /// The cell's label when our numbering knows it and no float took it yet.
    label: Option<String>,
    /// The id of a dropped float wrapper, for a float that has none.
    pending_id: Option<String>,
}

fn is_dropped_wrapper(e: &Elem) -> bool {
    e.name == "div"
        && (e.has_class("cell")
            || e.has_class("quarto-float")
            || e.has_class("quarto-figure")
            || e.attr("aria-describedby").is_some())
}

/// The class tokens an element keeps; `None` when they stay as written.
fn kept_classes(e: &Elem) -> Option<Vec<String>> {
    let class = e.attr("class")?;
    let tokens: Vec<&str> = class.split_whitespace().collect();
    let output = tokens.iter().any(|t| t.starts_with("cell-output"));
    let kept: Vec<String> = tokens
        .iter()
        .filter(|t| {
            if e.name == "div" && output {
                return t.starts_with("cell-output");
            }
            !(matches!(
                **t,
                "img-fluid"
                    | "figure-img"
                    | "figure"
                    | "anchored"
                    | "table"
                    | "small"
                    | "caption-top"
            ) || t.starts_with("quarto-")
                || t.starts_with("table-"))
        })
        .map(|t| t.to_string())
        .collect();
    (kept.len() != tokens.len()).then_some(kept)
}

fn open_tag_html(e: &Elem) -> String {
    let classes = kept_classes(e);
    let role = e.attr("role") == Some("img");
    if classes.is_none() && !role {
        return e.raw_open.clone();
    }
    let mut s = format!("<{}", e.name);
    for (key, value) in &e.attrs {
        if key == "role" && role {
            continue;
        }
        if key == "class" {
            match &classes {
                Some(k) if k.is_empty() => continue,
                Some(k) => {
                    s.push_str(&format!(" class=\"{}\"", k.join(" ")));
                    continue;
                }
                None => {}
            }
        }
        match value {
            Some(v) => s.push_str(&format!(" {key}=\"{}\"", v.replace('"', "&quot;"))),
            None => s.push_str(&format!(" {key}")),
        }
    }
    s.push_str(if e.raw_open.ends_with("/>") {
        " />"
    } else {
        ">"
    });
    s
}

fn write_nodes(nodes: &[Node], st: &mut State, out: &mut String) {
    for node in nodes {
        match node {
            Node::Text(t) | Node::Raw(t) => out.push_str(t),
            Node::Elem(e) => write_elem(e, st, out),
        }
    }
}

fn write_elem(e: &Elem, st: &mut State, out: &mut String) {
    if is_dropped_wrapper(e) {
        if st.pending_id.is_none() {
            st.pending_id = e.attr("id").map(str::to_string);
        }
        write_nodes(&e.children, st, out);
        return;
    }
    if e.name == "figure" && e.has_class("quarto-float") {
        let kind = if e.has_class("quarto-float-fig") {
            Some("fig")
        } else if e.has_class("quarto-float-tbl") {
            Some("tbl")
        } else {
            None
        };
        if let Some(kind) = kind {
            write_float(e, kind, st, out);
            return;
        }
    }
    out.push_str(&open_tag_html(e));
    write_nodes(&e.children, st, out);
    if let Some(close) = &e.close {
        out.push_str(close);
    }
}

fn is_caption(n: &Node) -> bool {
    matches!(n, Node::Elem(e) if e.name == "figcaption")
}

/// `Figure 1: A plot` -> `A plot`; `Table 1` -> ``. `None` when the text does
/// not start with a label.
fn strip_label(text: &str) -> Option<&str> {
    let t = text.trim_start();
    let digit = t.find(|c: char| c.is_ascii_digit())?;
    let word = &t[..digit];
    if word.is_empty()
        || word.len() > 40
        || !word
            .chars()
            .all(|c| c.is_alphabetic() || c == ' ' || c == '.')
    {
        return None;
    }
    let mut rest = &t[digit..];
    rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    while let Some(after) = rest.strip_prefix('.') {
        if !after.starts_with(|c: char| c.is_ascii_digit()) {
            break;
        }
        rest = after.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let rest = rest.strip_prefix([':', '.']).unwrap_or(rest);
    Some(rest.trim_start())
}

fn write_float(e: &Elem, kind: &str, st: &mut State, out: &mut String) {
    let quarto_id = e
        .attr("id")
        .map(str::to_string)
        .or_else(|| st.pending_id.take());
    st.pending_id = None;
    let ours = st.label.take_if(|l| l.starts_with(&format!("{kind}-")));
    let id = ours.clone().or(quarto_id);
    let caption_at = e.children.iter().position(is_caption);
    let mut body = String::new();
    for (n, child) in e.children.iter().enumerate() {
        if Some(n) != caption_at {
            write_nodes(std::slice::from_ref(child), st, &mut body);
        }
    }
    let mut caption = String::new();
    if let Some(Node::Elem(c)) = caption_at.map(|n| &e.children[n]) {
        let mut inner = String::new();
        match &ours {
            Some(label) => {
                let rest = match c.children.first() {
                    Some(Node::Text(t)) => match strip_label(t) {
                        Some(rest) => {
                            let mut tail = String::from(rest);
                            write_nodes(&c.children[1..], st, &mut tail);
                            tail
                        }
                        None => {
                            write_nodes(&c.children, st, &mut inner);
                            std::mem::take(&mut inner)
                        }
                    },
                    _ => {
                        write_nodes(&c.children, st, &mut inner);
                        std::mem::take(&mut inner)
                    }
                };
                inner = format!(
                    "<span class=\"caption-label\">{}</span>",
                    escape(&st.ctx.refs.caption_label(label))
                );
                let rest = rest.trim();
                if !rest.is_empty() {
                    inner.push(' ');
                    inner.push_str(rest);
                }
            }
            None => write_nodes(&c.children, st, &mut inner),
        }
        caption = format!("<figcaption>{}</figcaption>", inner.trim());
    }
    let class = if kind == "tbl" { "table" } else { "figure" };
    let id_attr = id
        .map(|id| format!(" id=\"{}\"", escape(&id)))
        .unwrap_or_default();
    out.push_str(&format!("<figure class=\"{class}\"{id_attr}>"));
    if kind == "tbl" {
        out.push_str(&caption);
        out.push_str(body.trim());
    } else {
        out.push_str(body.trim());
        out.push_str(&caption);
    }
    out.push_str("</figure>");
}

/// The saved HTML of `cell`'s output in the house vocabulary. HTML that does
/// not scan is returned as it was.
pub(crate) fn normalize(html: &str, cell: &Cell, ctx: &Context) -> String {
    let Some(nodes) = parse(html) else {
        return html.to_string();
    };
    let label =
        (!cell.label.is_empty() && ctx.refs.has_target(&cell.label)).then(|| cell.label.clone());
    let mut st = State {
        ctx,
        label,
        pending_id: None,
    };
    let mut out = String::with_capacity(html.len());
    write_nodes(&nodes, &mut st, &mut out);
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quarto::tests::{compose, with_outputs};

    const STDOUT: &str = r#"<div class="cell">  <div class="cell-output cell-output-stdout"> <pre><code>[1] 1 2 3</code></pre> </div> </div>"#;
    const FIG: &str = r#"<div class="cell">  <div class="cell-output-display"> <div id="fig-plot" class="quarto-float quarto-figure quarto-figure-center anchored"> <figure class="quarto-float quarto-float-fig figure"> <div aria-describedby="fig-plot-caption-0ab"> <img role="img" src="data:image/png;base64,iVBOR" class="img-fluid figure-img" width="672"> </div> <figcaption class="quarto-float-caption-bottom quarto-float-caption quarto-float-fig" id="fig-plot-caption-0ab"> Figure 1: A plot </figcaption> </figure> </div> </div> </div>"#;
    const TBL: &str = r#"<div id="tbl-head" class="cell quarto-float quarto-figure quarto-figure-center anchored"> <figure class="quarto-float quarto-float-tbl figure"> <figcaption class="quarto-float-caption-top quarto-float-caption quarto-float-tbl quarto-uncaptioned" id="tbl-head-caption-1cd"> Table 1 </figcaption> <div aria-describedby="tbl-head-caption-1cd">  <div class="cell-output cell-output-stdout"> <pre><code>a b</code></pre> </div> </div> </figure> </div>"#;

    fn cell_source(label: &str, cap: &str) -> String {
        format!("```{{r}}\n#| label: {label}\n#| {cap}\nx()\n```\n\nSee @{label}.\n")
    }

    fn shown(source: &str, html: &str) -> String {
        let json = format!(
            r#"{{"outputs":[{{"line":1,"html":{}}}]}}"#,
            serde_json::to_string(html).unwrap()
        );
        with_outputs(source, &json).markdown
    }

    #[test]
    fn stdout_loses_the_cell_wrapper_and_keeps_its_classes() {
        let d = shown("```{r}\nx()\n```\n", STDOUT);
        assert!(
            d.contains(
                "<div class=\"cell-output lp-cell-output\"><div class=\"cell-output cell-output-stdout\"> <pre><code>[1] 1 2 3</code></pre> </div></div>"
            ),
            "{d}"
        );
        assert!(!d.contains("class=\"cell\""), "{d}");
    }

    #[test]
    fn a_labelled_figure_gets_our_label_and_one_id() {
        let d = shown(&cell_source("fig-plot", "fig-cap: A plot"), FIG);
        let figure = "<figure class=\"figure\" id=\"fig-plot\"><img src=\"data:image/png;base64,iVBOR\" width=\"672\"><figcaption><span class=\"caption-label\">Figure 1.</span> A plot</figcaption></figure>";
        assert!(d.contains(figure), "{d}");
        assert_eq!(d.matches("id=\"fig-plot\"").count(), 1, "{d}");
        assert!(d.contains("href=\"#fig-plot\">Figure 1</a>"), "{d}");
        // The output is clean; the reference link keeps `quarto-crossref`.
        let output = &d[d.find("<div class=\"cell-output").unwrap()..d.find("See ").unwrap()];
        for gone in [
            "quarto-",
            "img-fluid",
            "figure-img",
            "role=",
            "anchored",
            "aria-",
        ] {
            assert!(!output.contains(gone), "{gone}: {output}");
        }
    }

    #[test]
    fn an_uncaptioned_table_float_is_label_only_with_the_caption_first() {
        let d = shown(&cell_source("tbl-head", "echo: false"), TBL);
        assert!(
            d.contains("<figure class=\"table\" id=\"tbl-head\"><figcaption><span class=\"caption-label\">Table 1.</span></figcaption><div class=\"cell-output cell-output-stdout\">"),
            "{d}"
        );
        assert!(d.contains("</div></figure>"), "{d}");
        assert_eq!(d.matches("id=\"tbl-head\"").count(), 1, "{d}");
    }

    #[test]
    fn numbering_follows_our_document_not_quarto() {
        // Two figures: the second cell's saved caption says Figure 1, ours says 2.
        let source = "```{r}\n#| label: fig-a\n#| fig-cap: A\nx()\n```\n\n```{r}\n#| label: fig-plot\n#| fig-cap: A plot\nx()\n```\n";
        let json = format!(
            r#"{{"outputs":[{{"line":7,"html":{}}}]}}"#,
            serde_json::to_string(FIG).unwrap()
        );
        let d = with_outputs(source, &json).markdown;
        assert!(
            d.contains("<span class=\"caption-label\">Figure 2.</span> A plot"),
            "{d}"
        );
    }

    #[test]
    fn without_a_label_of_ours_quarto_keeps_its_caption_and_id() {
        let d = shown("```{r}\nx()\n```\n", FIG);
        assert!(
            d.contains("<figure class=\"figure\" id=\"fig-plot\">")
                && d.contains("Figure 1: A plot"),
            "{d}"
        );
        assert!(!d.contains("caption-label"), "{d}");
    }

    #[test]
    fn bootstrap_table_classes_go() {
        let html = r#"<div class="cell"><div class="cell-output-display"><table class="table table-sm table-striped small"><caption class="caption-top">C</caption><tr><td>1</td></tr></table></div></div>"#;
        let d = shown("```{r}\nx()\n```\n", html);
        assert!(
            d.contains("<table><caption>C</caption><tr><td>1</td></tr></table>"),
            "{d}"
        );
        assert!(d.contains("cell-output-display"), "{d}");
    }

    #[test]
    fn stderr_display_math_and_unknown_markup_pass_through() {
        let html = r#"<div class="cell"><div class="cell-output cell-output-stderr"><pre><code>Warning</code></pre></div><div class="cell-output-display"><p>x <span class="math inline">\(a\)</span></p><custom-el data-x='1'>t</custom-el></div></div>"#;
        let d = shown("```{r}\nx()\n```\n", html);
        for kept in [
            "<div class=\"cell-output cell-output-stderr\"><pre><code>Warning</code></pre></div>",
            "<p>x <span class=\"math inline\">\\(a\\)</span></p>",
            "<custom-el data-x='1'>t</custom-el>",
        ] {
            assert!(d.contains(kept), "{kept}: {d}");
        }
    }

    #[test]
    fn malformed_html_is_returned_unchanged() {
        for html in [
            "<div class=\"cell\"><p>open",
            "<div></span>",
            "<div class=\"cell",
            "<!-- never closed",
            "<a href=>x</a><div",
        ] {
            assert!(
                parse(html).is_none() || html == "<a href=>x</a><div",
                "{html}"
            );
        }
        let d = shown("```{r}\nx()\n```\n", "<div class=\"cell\"><b>open");
        assert!(d.contains("<div class=\"cell\"><b>open"), "{d}");
        // Text with stray angle brackets and non-ASCII does not panic.
        let d = shown("```{r}\nx()\n```\n", "<p>1 < 2 é 日本</p>");
        assert!(d.contains("<p>1 < 2 é 日本</p>"), "{d}");
    }

    #[test]
    fn the_label_prefix_is_stripped() {
        assert_eq!(strip_label(" Figure 1: A plot "), Some("A plot "));
        assert_eq!(strip_label("Table 2.1"), Some(""));
        assert_eq!(strip_label("Figure 3. Done"), Some("Done"));
        assert_eq!(strip_label("A plot"), None);
    }

    #[test]
    fn a_labelled_cell_has_exactly_one_element_with_its_id() {
        let source = cell_source("fig-plot", "fig-cap: A plot");
        // No saved output: an empty anchor before the code.
        let d = compose(&source).markdown;
        assert_eq!(d.matches("id=\"fig-plot\"").count(), 1, "{d}");
        assert!(d.find("<span id=\"fig-plot\"></span>").unwrap() < d.find("```r").unwrap());
        assert!(d.contains("href=\"#fig-plot\">Figure 1</a>"), "{d}");
        // An output that does not carry the id (plain text): still the anchor.
        let d = shown(&source, STDOUT);
        assert_eq!(d.matches("id=\"fig-plot\"").count(), 1, "{d}");
        // A saved float carries it: no anchor.
        let d = shown(&source, FIG);
        assert_eq!(d.matches("id=\"fig-plot\"").count(), 1, "{d}");
        assert!(!d.contains("<span id=\"fig-plot\">"), "{d}");
    }

    /// The code part of a draft: everything before the saved output.
    fn code_part(markdown: &str) -> &str {
        markdown
            .split("<div class=\"cell-output")
            .next()
            .unwrap()
            .trim_end()
    }

    #[test]
    fn code_options_show_the_same_code_with_and_without_output() {
        let options = [
            "",
            "#| echo: false\n",
            "#| filename: a.R\n",
            "#| code-fold: true\n",
            "#| code-fold: show\n#| code-summary: Look\n",
            "#| output: false\n",
            "#| include: false\n",
            "#| eval: false\n",
            "#| echo: false\n#| output: false\n",
        ];
        for opt in options {
            let source = format!("```{{r}}\n{opt}shown_code()\n```\n");
            let without = compose(&source);
            let with = shown(&source, STDOUT);
            assert_eq!(
                code_part(&with),
                without.markdown.trim_end(),
                "{opt:?}\n{with}"
            );
        }
    }
}
