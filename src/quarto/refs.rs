//! Cross-references: numbering, resolution, heading ids.
//!
//! Owner: r857. [`references`] is the pre-pass: it numbers every target in
//! document order (figures, tables, equations, theorems, listings, sections),
//! works out section numbers, captions and the table of contents, and keeps
//! all of it in [`Refs`]. [`crossrefs`] turns `@fig-x` into a link. The hooks
//! let this module write a labelled div ([`div_open`], [`div_close`]), a
//! heading ([`decorate_heading`]), a line-level construct ([`line`]: figure
//! images, table captions, labelled equations) and the contents ([`toc`])
//! without the driver knowing about any of them.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::page::escape;

use super::attrs::parse_attributes;
use super::cells::option;
use super::divs::{self, Div};
use super::front_matter::{get, Map, Yaml};
use super::layout::{self, CapLocation, CellPlan};
use super::{backtick_run, code_span_end, fence_closes, fence_open, prose, Context, Out, Parsed};

// ------------------------------------------------------------- references

/// Id prefixes of numbered targets: figures, tables, sections, equations,
/// listings, the theorem family and the numbered callouts.
const PREFIXES: [&str; 20] = [
    "fig", "tbl", "sec", "eq", "lst", "thm", "lem", "cor", "prp", "cnj", "def", "exm", "exr",
    "sol", "rem", "nte", "tip", "wrn", "cau", "imp",
];

/// The theorem family: numbered per kind, rendered as `div.theorem`.
const THEOREMS: [&str; 10] = [
    "thm", "lem", "cor", "prp", "cnj", "def", "exm", "exr", "sol", "rem",
];

fn prefix_kind(id: &str) -> Option<&'static str> {
    PREFIXES
        .into_iter()
        .find(|k| id.strip_prefix(k).is_some_and(|rest| rest.starts_with('-')))
}

fn default_word(kind: &str) -> &'static str {
    match kind {
        "fig" => "Figure",
        "tbl" => "Table",
        "sec" => "Section",
        "eq" => "Equation",
        "lst" => "Listing",
        "thm" => "Theorem",
        "lem" => "Lemma",
        "cor" => "Corollary",
        "prp" => "Proposition",
        "cnj" => "Conjecture",
        "def" => "Definition",
        "exm" => "Example",
        "exr" => "Exercise",
        "sol" => "Solution",
        "rem" => "Remark",
        "nte" => "Note",
        "tip" => "Tip",
        "wrn" => "Warning",
        "cau" => "Caution",
        "imp" => "Important",
        "proof" => "Proof",
        _ => "",
    }
}

/// A custom float kind of `crossref: custom:`: `::: {#vid-x}` is numbered
/// and referred to like a figure.
struct Custom {
    key: String,
    /// `reference-prefix`: what a reference says (`Video 1`).
    name: String,
    /// `caption-prefix`: what the caption label says; the name by default.
    prefix: String,
}

/// The words of the `crossref:` front matter key: `fig-prefix` in a
/// reference, `fig-title` in a caption label, `title-delim` after it, and
/// the options that shape numbers and references.
#[derive(Default)]
struct Words {
    map: Map,
    customs: Vec<Custom>,
}

impl Words {
    fn new(meta: &Map, lines: &[&str], front: usize) -> Words {
        Words {
            map: match get(meta, "crossref") {
                Some(Yaml::Map(m)) => m.clone(),
                _ => Vec::new(),
            },
            customs: custom_kinds(&lines[..front.min(lines.len())]),
        }
    }

    fn lookup(&self, key: &str) -> Option<String> {
        get(&self.map, key).map(Yaml::text)
    }

    fn custom(&self, kind: &str) -> Option<&Custom> {
        self.customs.iter().find(|c| c.key == kind)
    }

    /// The kind an id belongs to: a built-in prefix or a custom key.
    fn kind_of(&self, id: &str) -> Option<String> {
        prefix_kind(id).map(str::to_string).or_else(|| {
            self.customs
                .iter()
                .find(|c| id.strip_prefix(c.key.as_str()).is_some_and(|r| r.starts_with('-')))
                .map(|c| c.key.clone())
        })
    }

    fn title(&self, kind: &str) -> String {
        self.lookup(&format!("{kind}-title")).unwrap_or_else(|| {
            self.custom(kind)
                .map_or_else(|| default_word(kind).to_string(), |c| c.prefix.clone())
        })
    }

    fn prefix(&self, kind: &str) -> String {
        self.lookup(&format!("{kind}-prefix")).unwrap_or_else(|| {
            self.custom(kind)
                .map_or_else(|| default_word(kind).to_string(), |c| c.name.clone())
        })
    }

    fn delimiter(&self) -> String {
        self.lookup("title-delim")
            .unwrap_or_else(|| ".".to_string())
    }

    /// Between the references of one citation group.
    fn ref_delim(&self) -> String {
        self.lookup("ref-delim").unwrap_or_else(|| ",".to_string())
    }

    /// Whether a reference is a link; `ref-hyperlink: false` makes it text.
    fn hyperlinks(&self) -> bool {
        get(&self.map, "ref-hyperlink").is_none_or(Yaml::truthy)
    }

    /// `chapters: true`: numbers start with the number of the chapter.
    fn chapters(&self) -> bool {
        get(&self.map, "chapters").is_some_and(Yaml::truthy)
    }

    /// The number `n` of a `kind` as its label style writes it: `labels` for
    /// all, `fig-labels` and so on for one kind, `subref-labels` for
    /// subfigures. `chapter` is put in front.
    fn number(&self, kind: &str, n: usize, chapter: Option<&str>) -> String {
        let default = if kind == "subref" { "alpha a" } else { "arabic" };
        let style = self
            .lookup(&format!("{kind}-labels"))
            .or_else(|| (kind != "subref").then(|| self.lookup("labels")).flatten())
            .unwrap_or_else(|| default.to_string());
        let style = style.trim();
        let text = if let Some(rest) = style.strip_prefix("alpha") {
            let start = rest.trim().chars().next().unwrap_or('a');
            char::from_u32(start as u32 + n.max(1) as u32 - 1)
                .unwrap_or(start)
                .to_string()
        } else if style.starts_with("roman") || style.starts_with("Roman") {
            // Quarto: lower case only when the style ends in `i`.
            roman(n, style.ends_with('i'))
        } else {
            n.to_string()
        };
        match chapter {
            Some(c) => format!("{c}.{text}"),
            None => text,
        }
    }
}

fn roman(mut n: usize, lower: bool) -> String {
    const NUMERALS: [(usize, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, text) in NUMERALS {
        while n >= value {
            out.push_str(text);
            n -= value;
        }
    }
    if lower {
        out.to_lowercase()
    } else {
        out
    }
}

/// The `crossref: custom:` entries of the front matter, from the lines
/// themselves: the front matter reader keeps no lists of maps. Both the flow
/// form (`custom: [{kind: float, key: vid, reference-prefix: Video}]`) and
/// the block form are read. Only floats with a key count.
fn custom_kinds(front: &[&str]) -> Vec<Custom> {
    let unquote = |v: &str| v.trim().trim_matches(['"', '\'']).to_string();
    let mut entries: Vec<Vec<(String, String)>> = Vec::new();
    let Some(at) = front.iter().position(|l| l.trim_start().starts_with("custom:")) else {
        return Vec::new();
    };
    let indent = front[at].len() - front[at].trim_start().len();
    let rest = front[at].trim_start()["custom:".len()..].trim();
    if rest.starts_with('[') {
        // `[{a: b, c: d}, {...}]`: the braces hold the entries.
        let mut depth = 0;
        let mut from = 0;
        for (i, c) in rest.char_indices() {
            match c {
                '{' => {
                    if depth == 0 {
                        from = i + 1;
                    }
                    depth += 1;
                }
                '}' if depth > 0 => {
                    depth -= 1;
                    if depth == 0 {
                        entries.push(
                            rest[from..i]
                                .split(',')
                                .filter_map(|kv| kv.split_once(':'))
                                .map(|(k, v)| (k.trim().to_string(), unquote(v)))
                                .collect(),
                        );
                    }
                }
                _ => {}
            }
        }
    } else {
        for line in &front[at + 1..] {
            let t = line.trim_start();
            if t.is_empty() {
                continue;
            }
            if line.len() - t.len() <= indent && !t.starts_with("- ") {
                break;
            }
            let (item, body) = match t.strip_prefix("- ") {
                Some(body) => (true, body),
                None => (false, t),
            };
            if item {
                entries.push(Vec::new());
            }
            if let (Some(entry), Some((k, v))) = (entries.last_mut(), body.split_once(':')) {
                entry.push((k.trim().to_string(), unquote(v)));
            }
        }
    }
    entries
        .into_iter()
        .filter_map(|entry| {
            let field = |k: &str| entry.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            if field("kind").is_some_and(|k| k != "float") {
                return None;
            }
            let key = field("key").filter(|k| !k.is_empty())?;
            let name = field("reference-prefix")
                .or_else(|| field("caption-prefix"))
                .unwrap_or_else(|| key.clone());
            let prefix = field("caption-prefix").unwrap_or_else(|| name.clone());
            Some(Custom { key, name, prefix })
        })
        .collect()
}

/// A numbered target: `1`, `2.1` (a section), `A.1` (an appendix section) or
/// `1b` (a subfigure).
struct Target {
    kind: String,
    number: String,
}

struct Heading {
    /// The line as the source has it; a decoration applies only to that.
    raw: String,
    level: usize,
    title: String,
    id: String,
    number: Option<String>,
    decorate: bool,
    /// Written as a title line and an underline; the draft makes it ATX.
    setext: bool,
}

/// What a div needs at its fences: the caption it gave up, a theorem's name
/// and where its label goes.
#[derive(Default)]
struct DivInfo {
    id: String,
    kind: String,
    name: String,
    caption: String,
    /// The first line of content, when the label can start it.
    inline_line: Option<usize>,
}

struct Image {
    id: String,
    src: String,
    alt: String,
    fig_alt: String,
    fig_link: String,
    width: String,
    height: String,
    align: String,
    /// Every attribute of the image, for `cap-location` and `fig-column`.
    attrs: Map,
}

enum Item {
    Image(Image),
    Equation {
        id: String,
        tex: String,
    },
    /// A pipe table with its caption line, before or after it.
    Table {
        id: String,
        caption: String,
        rows: Range<usize>,
    },
}

/// A construct of one or more lines, found at its first line.
struct Span {
    item: Item,
    /// The last line it covers.
    end: usize,
}

/// Everything the pre-pass found: the numbered targets, section numbers,
/// captions and the contents. Read-only while the draft is written.
#[derive(Default)]
pub(crate) struct Refs {
    targets: HashMap<String, Target>,
    words: Words,
    /// By line.
    headings: HashMap<usize, Heading>,
    /// By the line of the opening fence.
    divs: HashMap<usize, DivInfo>,
    /// By the first line.
    items: HashMap<usize, Span>,
    /// Lines that moved elsewhere: captions, theorem names.
    skip: HashSet<usize>,
    /// First content line of a div -> the line of its opening fence.
    inline: HashMap<usize, usize>,
    /// Subfigure id -> `a`, `b`... (in the `subref-labels` style).
    letters: HashMap<String, String>,
    /// By the first line of a cell of a `layout="[[1,1],[1]]"` figure: its
    /// row breaks, spacers and width.
    plans: HashMap<usize, CellPlan>,
    /// The front matter, for the document-level layout options.
    meta: Map,
    toc: String,
}

impl Refs {
    /// `Figure 1`: what a reference to `id` says. `bare` leaves the prefix
    /// out (`[-@fig-x]`), `custom` replaces it (`[Fig. @fig-x]`) and `upper`
    /// capitalises it (`@Fig-x`).
    fn reference_text(
        &self,
        id: &str,
        bare: bool,
        custom: Option<&str>,
        upper: bool,
    ) -> Option<String> {
        let target = self.targets.get(id)?;
        let mut prefix = match (bare, custom) {
            (true, _) => String::new(),
            (false, Some(text)) => text.to_string(),
            (false, None) => self.words.prefix(&target.kind),
        };
        if upper {
            let mut chars = prefix.chars();
            prefix = chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
        }
        Some(if prefix.is_empty() {
            target.number.clone()
        } else {
            format!("{prefix} {}", target.number)
        })
    }

    /// `Note 1` for a numbered callout, the start of its title.
    pub(crate) fn callout_label(&self, id: &str) -> Option<String> {
        let target = self.targets.get(id)?;
        matches!(target.kind.as_str(), "nte" | "tip" | "wrn" | "cau" | "imp")
            .then(|| format!("{} {}", self.words.title(&target.kind), target.number))
    }

    /// The delimiter between a caption label and its text.
    pub(crate) fn title_delimiter(&self) -> String {
        self.words.delimiter()
    }

    /// Whether `id` is a numbered target of the document.
    pub(crate) fn has_target(&self, id: &str) -> bool {
        self.targets.contains_key(id)
    }

    /// `Figure 1.` or `(a)`: what a caption starts with.
    pub(crate) fn caption_label(&self, id: &str) -> String {
        if let Some(letter) = self.letters.get(id) {
            return format!("({letter})");
        }
        let delimiter = self.words.delimiter();
        match self.targets.get(id) {
            Some(t) => format!("{} {}{delimiter}", self.words.title(&t.kind), t.number),
            None => format!(
                "{}{delimiter}",
                self.words
                    .title(&self.words.kind_of(id).unwrap_or_else(|| "fig".into()))
            ),
        }
    }

    /// Where the caption of a float of `kind` goes.
    pub(crate) fn cap_location(&self, element: &Map, kind: &str) -> CapLocation {
        layout::cap_location(element, &self.meta, kind)
    }

    /// The wide-column class of a float of `kind`, from its `{kind}-column`.
    pub(crate) fn column_class(&self, element: &Map, kind: &str) -> Option<String> {
        layout::column_class(element, &self.meta, kind)
    }

    /// `Theorem 1 (Name).` as spans, or `Proof.`.
    fn div_label(&self, info: &DivInfo) -> String {
        if info.kind == "proof" {
            return format!(
                "<span class=\"proof-label\">{}.</span>",
                escape(&self.words.title("proof"))
            );
        }
        let title = self.words.title(&info.kind);
        let head = match self.targets.get(&info.id) {
            Some(t) if !info.id.is_empty() => format!("{title} {}", t.number),
            _ => title,
        };
        let mut out = format!("<span class=\"theorem-label\">{}</span>", escape(&head));
        if !info.name.is_empty() {
            out.push_str(&format!(
                " <span class=\"theorem-name\">({})</span>",
                inline_html(&info.name, self)
            ));
        }
        out.push('.');
        out
    }

    /// Where a caption goes is the caller's business; a margin caption says
    /// so with a class.
    fn figcaption_in(&self, id: &str, caption: &str, location: CapLocation) -> String {
        let class = if location == CapLocation::Margin {
            " class=\"margin-caption\""
        } else {
            ""
        };
        let mut out = format!(
            "<figcaption{class}><span class=\"caption-label\">{}</span>",
            escape(&self.caption_label(id))
        );
        if !caption.is_empty() {
            out.push(' ');
            out.push_str(&inline_html(caption, self));
        }
        out.push_str("</figcaption>");
        out
    }

    /// Whether `kind` is a float of `crossref: custom:`.
    fn is_custom(&self, kind: &str) -> bool {
        self.words.custom(kind).is_some()
    }

    fn image_html(&self, img: &Image, plan: Option<&CellPlan>) -> String {
        let class = if self.letters.contains_key(&img.id) {
            "subfigure"
        } else {
            "figure"
        };
        let alt = if img.fig_alt.is_empty() {
            plain(&img.alt)
        } else {
            img.fig_alt.clone()
        };
        let mut style = Vec::new();
        if !img.width.is_empty() {
            style.push(format!("width:{}", img.width));
        }
        if !img.height.is_empty() {
            style.push(format!("height:{}", img.height));
        }
        match img.align.as_str() {
            "center" => style.push("display:block;margin-left:auto;margin-right:auto".into()),
            "right" => style.push("display:block;margin-left:auto".into()),
            "left" => style.push("display:block;margin-right:auto".into()),
            _ => {}
        }
        let style = if style.is_empty() {
            String::new()
        } else {
            format!(" style=\"{}\"", escape(&style.join(";")))
        };
        let location = self.cap_location(&img.attrs, "fig");
        let caption = self.figcaption_in(&img.id, &img.alt, location);
        let (above, below) = if location == CapLocation::Top {
            (caption, String::new())
        } else {
            (String::new(), caption)
        };
        let mut classes = vec![class.to_string()];
        classes.extend(self.column_class(&img.attrs, "fig"));
        let (lead, cell_style) = match plan {
            Some(p) => (
                p.before.as_str(),
                format!(" style=\"flex-basis:{}\"", p.width),
            ),
            None => ("", String::new()),
        };
        let image = format!(
            "<img src=\"{src}\" alt=\"{alt}\"{style}>",
            src = escape(&img.src),
            alt = escape(&alt),
        );
        // `fig-link` wraps the image in a link.
        let image = if img.fig_link.is_empty() {
            image
        } else {
            format!("<a href=\"{}\">{image}</a>", escape(&img.fig_link))
        };
        format!(
            "{lead}<figure class=\"{classes}\" id=\"{id}\"{cell_style}>{above}{image}{below}</figure>",
            classes = classes.join(" "),
            id = escape(&img.id),
        )
    }

    fn equation_html(&self, id: &str, tex: &str) -> String {
        let number = self.targets.get(id).map_or("", |t| t.number.as_str());
        format!(
            "<div class=\"equation\" id=\"{}\"><span data-math-style=\"display\">{}</span><span class=\"equation-number\">({})</span></div>",
            escape(id),
            escape(tex),
            escape(number)
        )
    }

    /// The opening of a table float; the caption is here when it goes above.
    fn table_open_in(
        &self,
        id: &str,
        caption: &str,
        location: CapLocation,
        classes: &[String],
    ) -> String {
        let mut class = vec!["table".to_string()];
        class.extend(classes.iter().cloned());
        let above = if location == CapLocation::Top {
            self.figcaption_in(id, caption, location)
        } else {
            String::new()
        };
        format!(
            "<figure class=\"{}\" id=\"{}\">\n{above}",
            class.join(" "),
            escape(id)
        )
    }

    /// The end of a table float; the caption is here when it goes below or
    /// in the margin.
    fn table_close(&self, id: &str, caption: &str, location: CapLocation) -> String {
        if location == CapLocation::Top {
            "</figure>".to_string()
        } else {
            format!("{}</figure>", self.figcaption_in(id, caption, location))
        }
    }
}

/// One paragraph of Markdown as inline HTML, cross references resolved.
fn inline_html(text: &str, refs: &Refs) -> String {
    let html = crate::markdown::render_body(&crossrefs(text, refs, false));
    let html = html.trim();
    let html = html
        .strip_prefix("<p>")
        .and_then(|h| h.strip_suffix("</p>"))
        .unwrap_or(html);
    html.replace("\n\n", "\n")
}

/// The character an HTML entity at the start of `text` stands for, and how
/// many characters the entity takes, the way comrak reads it: the named
/// entities that matter in prose and numeric ones.
fn entity_at(text: &[char]) -> Option<(char, usize)> {
    if text.first() != Some(&'&') {
        return None;
    }
    let end = text.iter().take(32).position(|&c| c == ';')?;
    let name: String = text[1..end].iter().collect();
    let c = match name.as_str() {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            if digits.is_empty() || code == 0 {
                '\u{fffd}'
            } else {
                char::from_u32(code).unwrap_or('\u{fffd}')
            }
        }
    };
    Some((c, end + 1))
}

/// The text of some inline Markdown, the way comrak collects it for a
/// heading id or an `alt`: emphasis marks, link syntax and backslashes are
/// gone, code is literal and entities are decoded.
fn plain(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '`' => {
                let run = chars[i..].iter().take_while(|&&c| c == '`').count();
                let mut j = i + run;
                let mut close = None;
                while j < chars.len() {
                    if chars[j] == '`' {
                        let m = chars[j..].iter().take_while(|&&c| c == '`').count();
                        if m == run {
                            close = Some(j);
                            break;
                        }
                        j += m;
                    } else {
                        j += 1;
                    }
                }
                match close {
                    Some(j) => {
                        let mut code: String = chars[i + run..j].iter().collect();
                        if code.len() > 2
                            && code.starts_with(' ')
                            && code.ends_with(' ')
                            && code.trim() != ""
                        {
                            code = code[1..code.len() - 1].to_string();
                        }
                        out.push_str(&code);
                        i = j + run;
                    }
                    None => {
                        out.extend(std::iter::repeat_n('`', run));
                        i += run;
                    }
                }
            }
            '\\' => match chars.get(i + 1) {
                Some(&c) if c.is_ascii_punctuation() => {
                    out.push(c);
                    i += 2;
                }
                _ => {
                    out.push('\\');
                    i += 1;
                }
            },
            '&' => match entity_at(&chars[i..]) {
                Some((c, taken)) => {
                    out.push(c);
                    i += taken;
                }
                None => {
                    out.push('&');
                    i += 1;
                }
            },
            '*' | '[' => i += 1,
            '_' => {
                // Intraword underscores are literal; the others are emphasis.
                let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
                if word(i.checked_sub(1).and_then(|k| chars.get(k))) && word(chars.get(i + 1)) {
                    out.push('_');
                }
                i += 1;
            }
            ']' => {
                i += 1;
                if chars.get(i) == Some(&'(') {
                    while i < chars.len() && chars[i] != ')' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// What comrak's anchorizer is given for a heading: dashes and ellipses that
/// "smart" punctuation turns into characters the anchorizer drops.
fn slug_text(title: &str) -> String {
    plain(title)
        .replace("---", "")
        .replace("--", "")
        .replace("...", "")
}

// ---------------------------------------------------------------- parsing

/// `Title {#id .class}`: the title and what is inside the braces.
fn split_attrs(text: &str) -> (&str, Option<&str>) {
    let t = text.trim_end();
    if !t.ends_with('}') {
        return (t, None);
    }
    let Some(k) = t.rfind('{') else {
        return (t, None);
    };
    let (before, inner) = (&t[..k], t[k + 1..t.len() - 1].trim());
    let looks_like_attributes = inner.starts_with(['#', '.', '-']) || inner.contains('=');
    if !looks_like_attributes || !(before.is_empty() || before.ends_with(char::is_whitespace)) {
        return (t, None);
    }
    (before.trim_end(), Some(inner))
}

struct HeadingLine {
    level: usize,
    title: String,
    id: String,
    classes: Vec<String>,
    unnumbered: bool,
    has_attrs: bool,
    setext: bool,
}

fn parse_heading(line: &str) -> Option<HeadingLine> {
    if !is_heading(line) {
        return None;
    }
    let t = line.trim_start_matches(' ');
    let level = t.bytes().take_while(|&b| b == b'#').count();
    let (title, attrs) = split_attrs(t[level..].trim());
    let mut out = HeadingLine {
        level,
        title: title.to_string(),
        id: String::new(),
        classes: Vec::new(),
        unnumbered: false,
        has_attrs: false,
        setext: false,
    };
    if let Some(inner) = attrs {
        let parsed = parse_attributes(inner);
        out.unnumbered = parsed.classes.iter().any(|c| c == "unnumbered")
            || inner.split_whitespace().any(|w| w == "-");
        out.id = parsed.id;
        out.classes = parsed.classes;
        out.has_attrs = true;
    }
    Some(out)
}

/// A setext heading: a plain title line, then `===` (level 1) or `---`
/// (level 2) under it, after a blank line or a div fence.
fn parse_setext(lines: &[&str], live: &[bool], front: usize, i: usize) -> Option<HeadingLine> {
    let under = lines.get(i + 1)?;
    if !live[i] || !live.get(i + 1).copied().unwrap_or(false) || !is_plain_paragraph(lines[i]) {
        return None;
    }
    let after_gap = i <= front
        || lines[i - 1].trim().is_empty()
        || divs::div_fence(lines[i - 1]).is_some();
    let t = under.trim_end();
    let lead = t.len() - t.trim_start().len();
    let t = t.trim_start();
    let level = match t.chars().next()? {
        '=' if t.len() >= 2 && t.bytes().all(|b| b == b'=') => 1,
        '-' if t.len() >= 2 && t.bytes().all(|b| b == b'-') => 2,
        _ => return None,
    };
    if !after_gap || lead > 3 {
        return None;
    }
    let parsed = parse_heading(&format!("{} {}", "#".repeat(level), lines[i].trim()))?;
    Some(HeadingLine {
        setext: true,
        ..parsed
    })
}

/// Lines outside the front matter and outside code fences.
fn live_lines(lines: &[&str], front: usize) -> Vec<bool> {
    let mut live = vec![false; lines.len()];
    let mut i = front;
    while i < lines.len() {
        if let Some((ch, len, _)) = fence_open(lines[i]) {
            i += 1;
            while i < lines.len() && !fence_closes(lines[i], ch, len) {
                i += 1;
            }
            i += 1;
            continue;
        }
        live[i] = true;
        i += 1;
    }
    live
}

/// Whether a line starts a paragraph that is plain text (not a list, a
/// quote, a table, HTML, a fence, an image...).
fn is_plain_paragraph(line: &str) -> bool {
    let t = line.trim();
    let lead = line.len() - line.trim_start_matches(' ').len();
    if t.is_empty() || lead > 3 || t.contains("{{<") {
        return false;
    }
    let digits = t.bytes().take_while(|b| b.is_ascii_digit()).count();
    let ordered = digits > 0
        && matches!(t.as_bytes().get(digits).copied(), Some(b'.' | b')'))
        && t[digits + 1..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace);
    let first = t.chars().next().unwrap_or(' ');
    !(ordered
        || matches!(first, '#' | '>' | '|' | '<' | '-' | '+' | ':' | '=')
        || (first == '*' && (t[1..].starts_with(' ') || t.starts_with("***")))
        || (first == '_' && t.starts_with("___"))
        || t.starts_with("```")
        || t.starts_with("~~~")
        || t.starts_with("$$")
        || t.starts_with("![")
        || t.starts_with("[^"))
}

/// The last paragraph of a div's content: its text and its lines.
fn last_paragraph(
    lines: &[&str],
    live: &[bool],
    content: &Range<usize>,
) -> Option<(String, Range<usize>)> {
    let mut end = content.end;
    while end > content.start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let mut start = end;
    while start > content.start && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    if start == end || !is_plain_paragraph(lines[start]) {
        return None;
    }
    if (start..end).any(|l| !live[l] || divs::div_fence(lines[l]).is_some()) {
        return None;
    }
    let text: Vec<&str> = lines[start..end].iter().map(|l| l.trim()).collect();
    Some((text.join("\n"), start..end))
}

fn css_length(value: String) -> String {
    if !value.is_empty() && value.chars().all(|c| c.is_ascii_digit() || c == '.') {
        format!("{value}px")
    } else {
        value
    }
}

/// `![Caption](path){#fig-x width=50%}` alone on a line.
fn parse_image(line: &str) -> Option<Image> {
    let rest = line.trim().strip_prefix("![")?;
    let mut depth = 0usize;
    let mut close = None;
    for (k, c) in rest.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                if depth == 0 {
                    close = Some(k);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let close = close?;
    let alt = &rest[..close];
    let after = rest[close + 1..].strip_prefix('(')?;
    let mut depth = 0usize;
    let mut end = None;
    for (k, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    end = Some(k);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let end = end?;
    let mut destination = after[..end].trim();
    let tail = after[end + 1..].trim();
    if !tail.starts_with('{') || !tail.ends_with('}') {
        return None;
    }
    let attrs = parse_attributes(tail);
    if prefix_kind(&attrs.id) != Some("fig") {
        return None;
    }
    if destination.ends_with('"') {
        if let Some(k) = destination.rfind(" \"") {
            destination = destination[..k].trim_end();
        }
    }
    let destination = destination
        .strip_prefix('<')
        .and_then(|d| d.strip_suffix('>'))
        .unwrap_or(destination);
    let text = |key: &str| get(&attrs.attrs, key).map(Yaml::text).unwrap_or_default();
    Some(Image {
        id: attrs.id.clone(),
        src: destination.to_string(),
        alt: alt.to_string(),
        fig_alt: text("fig-alt"),
        fig_link: text("fig-link"),
        width: css_length(text("width")),
        height: css_length(text("height")),
        align: text("fig-align"),
        attrs: attrs.attrs.clone(),
    })
}

/// `{#eq-x}` after the closing `$$`.
fn equation_label(rest: &str) -> Option<String> {
    let rest = rest.trim();
    if !rest.starts_with('{') || !rest.ends_with('}') {
        return None;
    }
    let attrs = parse_attributes(rest);
    (prefix_kind(&attrs.id) == Some("eq")).then_some(attrs.id)
}

/// A labelled display equation starting on line `i`: its id, its TeX and the
/// line it ends on.
fn parse_equation(lines: &[&str], live: &[bool], i: usize) -> Option<(String, String, usize)> {
    let body = lines[i].trim().strip_prefix("$$")?;
    if let Some(close) = body.find("$$") {
        let id = equation_label(&body[close + 2..])?;
        return Some((id, body[..close].trim().to_string(), i));
    }
    let mut tex = vec![body.trim().to_string()];
    for j in i + 1..lines.len().min(i + 200) {
        let t = lines[j].trim();
        if !live[j] || t.is_empty() {
            return None;
        }
        if let Some(close) = t.find("$$") {
            tex.push(t[..close].trim().to_string());
            let id = equation_label(&t[close + 2..])?;
            tex.retain(|s| !s.is_empty());
            return Some((id, tex.join("\n"), j));
        }
        tex.push(t.to_string());
    }
    None
}

/// `: Caption {#tbl-x}`: the id and the caption.
fn parse_table_caption(line: &str) -> Option<(String, String)> {
    let rest = line.trim().strip_prefix(':')?;
    if !rest.starts_with(' ') {
        return None;
    }
    let rest = rest.trim();
    let k = rest.rfind('{')?;
    if !rest.ends_with('}') {
        return None;
    }
    let attrs = parse_attributes(&rest[k..]);
    (prefix_kind(&attrs.id) == Some("tbl")).then(|| (attrs.id, rest[..k].trim().to_string()))
}

fn is_row(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.contains('|')
}

fn is_delimiter(line: &str) -> bool {
    let t = line.trim();
    t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn table_ok(lines: &[&str], rows: Range<usize>) -> Option<Range<usize>> {
    (rows.len() >= 2 && is_delimiter(lines[rows.start + 1])).then_some(rows)
}

/// The pipe table just above a caption line (one blank line may part them).
fn table_above(lines: &[&str], live: &[bool], caption: usize) -> Option<Range<usize>> {
    let mut last = caption.checked_sub(1)?;
    if lines[last].trim().is_empty() {
        last = last.checked_sub(1)?;
    }
    if !(live[last] && is_row(lines[last])) {
        return None;
    }
    let mut start = last;
    while start > 0 && live[start - 1] && is_row(lines[start - 1]) {
        start -= 1;
    }
    table_ok(lines, start..last + 1)
}

/// The pipe table just below a caption line.
fn table_below(lines: &[&str], live: &[bool], caption: usize) -> Option<Range<usize>> {
    let mut first = caption + 1;
    if first < lines.len() && lines[first].trim().is_empty() {
        first += 1;
    }
    if first >= lines.len() || !live[first] || !is_row(lines[first]) {
        return None;
    }
    let mut end = first;
    while end < lines.len() && live[end] && is_row(lines[end]) {
        end += 1;
    }
    table_ok(lines, first..end)
}

fn class_kind(div: &Div) -> Option<&'static str> {
    div.classes.iter().find_map(|c| match c.as_str() {
        "proof" => Some("proof"),
        "remark" => Some("rem"),
        "solution" => Some("sol"),
        _ => None,
    })
}

fn is_appendix(div: &Div) -> bool {
    div.classes.iter().any(|c| c == "appendix")
}

fn letter(n: usize) -> String {
    char::from(b'A' + (n.max(1) - 1) as u8 % 26).to_string()
}

/// The contents: a nested list of links to the headings.
fn build_toc(items: &[(usize, String, Option<String>, String)], title: &str) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "<nav class=\"toc\"><p class=\"toc-title\">{}</p>",
        escape(title)
    );
    let mut open: Vec<usize> = Vec::new();
    for (depth, link, number, text) in items {
        match open.last().copied() {
            None => {
                out.push_str("<ol>");
                open.push(*depth);
            }
            Some(last) if *depth > last => {
                out.push_str("<ol>");
                open.push(*depth);
            }
            Some(_) => {
                out.push_str("</li>");
                while open.len() > 1 && open.last().is_some_and(|&l| *depth < l) {
                    out.push_str("</ol></li>");
                    open.pop();
                }
            }
        }
        out.push_str(&format!("<li><a href=\"#{}\">", escape(link)));
        if let Some(n) = number {
            out.push_str(&format!(
                "<span class=\"section-number\">{}</span> ",
                escape(n)
            ));
        }
        out.push_str(&escape(text));
        out.push_str("</a>");
    }
    out.push_str("</li>");
    while open.len() > 1 {
        out.push_str("</ol></li>");
        open.pop();
    }
    out.push_str("</ol></nav>");
    out
}

/// Every numbered target of the document, in document order by kind, with
/// section numbers, captions and the contents worked out.
pub(crate) fn references(parsed: &Parsed, lines: &[&str]) -> Refs {
    let meta = &parsed.fm.meta;
    let live = live_lines(lines, parsed.fm.lines);
    let mut refs = Refs {
        words: Words::new(meta, lines, parsed.fm.lines),
        meta: meta.clone(),
        ..Refs::default()
    };
    // (line, id) of everything that takes a number.
    let mut cands: Vec<(usize, String)> = Vec::new();
    // Subfigures: (line, id, index of the figure div they belong to).
    let mut subs: Vec<(usize, String, usize)> = Vec::new();
    let innermost = |line: usize| -> Option<usize> {
        parsed
            .divs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.content.contains(&line))
            .max_by_key(|(_, d)| d.depth)
            .map(|(n, _)| n)
    };

    // Divs: captions move to the figcaption, names to the label.
    for d in &parsed.divs {
        let pk = refs.words.kind_of(&d.id);
        if pk.is_some() {
            let parent = d.parent.filter(|&p| {
                pk.as_deref() == Some("fig") && prefix_kind(&parsed.divs[p].id) == Some("fig")
            });
            match parent {
                Some(p) => subs.push((d.start, d.id.clone(), p)),
                None => cands.push((d.start, d.id.clone())),
            }
        }
        let kind: Option<String> = match pk {
            Some(k)
                if k == "fig"
                    || k == "tbl"
                    || THEOREMS.contains(&k.as_str())
                    || refs.is_custom(&k) =>
            {
                Some(k)
            }
            Some(_) => None,
            None => class_kind(d).map(str::to_string),
        };
        let Some(kind) = kind else { continue };
        let mut info = DivInfo {
            id: d.id.clone(),
            kind,
            ..DivInfo::default()
        };
        if info.kind == "fig" || info.kind == "tbl" || refs.is_custom(&info.kind) {
            if let Some((text, range)) = last_paragraph(lines, &live, &d.content) {
                info.caption = text;
                refs.skip.extend(range);
            }
        } else {
            let mut from = d.content.start;
            while from < d.content.end && lines[from].trim().is_empty() {
                from += 1;
            }
            if from < d.content.end && live[from] {
                if let Some(h) = parse_heading(lines[from]) {
                    info.name = h.title;
                    refs.skip.insert(from);
                    from += 1;
                }
            }
            if let Some(name) = get(&d.attrs, "name") {
                info.name = name.text();
            }
            while from < d.content.end && lines[from].trim().is_empty() {
                from += 1;
            }
            if from < d.content.end && live[from] && is_plain_paragraph(lines[from]) {
                info.inline_line = Some(from);
                refs.inline.insert(from, d.start);
            }
        }
        refs.divs.insert(d.start, info);
    }

    // Headings: section numbers, anchors, the contents.
    // `number-offset` (a number or a list) implies `number-sections`; the
    // counters start at the offsets, as Pandoc's do.
    let offsets: Vec<usize> = match get(meta, "number-offset") {
        Some(Yaml::Num(n)) => vec![n.max(0.0) as usize],
        Some(Yaml::List(items)) => items
            .iter()
            .map(|y| match y {
                Yaml::Num(n) => n.max(0.0) as usize,
                _ => 0,
            })
            .collect(),
        _ => Vec::new(),
    };
    let number_sections =
        get(meta, "number-sections").is_some_and(|y| y.truthy()) || !offsets.is_empty();
    // `shift-heading-level-by` moves every heading, clamped to 1..6.
    let shift = match get(meta, "shift-heading-level-by") {
        Some(Yaml::Num(n)) => *n as i64,
        _ => 0,
    };
    let depth_limit = match get(meta, "number-depth") {
        Some(Yaml::Num(n)) if *n >= 1.0 => *n as usize,
        _ => usize::MAX,
    };
    let toc_on = get(meta, "toc").is_some_and(|y| y.truthy());
    let toc_depth = match get(meta, "toc-depth") {
        Some(Yaml::Num(n)) if *n >= 1.0 => *n as usize,
        _ => 3,
    };
    let toc_title = match get(meta, "toc-title") {
        Some(y) if y.truthy() => y.text(),
        _ => "Contents".to_string(),
    };
    // Headings a div consumes (callout titles, tab headings) are not sections.
    let consumed: HashSet<usize> = parsed
        .divs
        .iter()
        .flat_map(|d| divs::consumed_headings(d, lines))
        .collect();
    let mut headings: Vec<(usize, HeadingLine)> = Vec::new();
    for i in 0..lines.len() {
        if !live[i] || refs.skip.contains(&i) || consumed.contains(&i) {
            continue;
        }
        let found = parse_heading(lines[i])
            .or_else(|| parse_setext(lines, &live, parsed.fm.lines, i));
        if let Some(h) = found {
            headings.push((i, h));
        }
    }
    let base = headings.iter().map(|(_, h)| h.level).min().unwrap_or(1);
    let appendix_divs: Vec<(usize, Range<usize>)> = parsed
        .divs
        .iter()
        .enumerate()
        .filter(|(_, d)| is_appendix(d))
        .map(|(n, d)| (n, d.content.clone()))
        .collect();
    let appendix_from = headings
        .iter()
        .find(|(_, h)| h.classes.iter().any(|c| c == "appendix"))
        .map(|(line, _)| *line);
    let region_of = |line: usize| -> Option<usize> {
        appendix_divs
            .iter()
            .find(|(_, range)| range.contains(&line))
            .map(|(n, _)| *n)
            .or_else(|| {
                appendix_from
                    .filter(|&from| line >= from)
                    .map(|_| usize::MAX)
            })
    };
    let mut start_counters = [0usize; 6];
    for (slot, offset) in start_counters.iter_mut().zip(&offsets) {
        *slot = *offset;
    }
    let mut counters = start_counters;
    let mut region: Option<usize> = None;
    let mut appendix_level = base;
    let mut section_numbers: HashMap<String, String> = HashMap::new();
    // Every heading in order, with its depth in the contents when listed.
    let mut toc_rows: Vec<TocRow> = Vec::new();
    // `chapters: true`: where each chapter starts and what it is called.
    let mut chapter_marks: Vec<(usize, String)> = Vec::new();
    let mut chapter_count = 0;
    for (line, h) in &headings {
        let key = region_of(*line);
        if key != region {
            region = key;
            counters = if key.is_some() { [0; 6] } else { start_counters };
            appendix_level = h.level;
            chapter_count = 0;
        }
        let appendix = key.is_some();
        let depth = h
            .level
            .saturating_sub(if appendix { appendix_level } else { base })
            + 1;
        if depth == 1 && !h.unnumbered {
            chapter_count += 1;
            chapter_marks.push((
                *line,
                if appendix {
                    letter(chapter_count)
                } else {
                    chapter_count.to_string()
                },
            ));
        }
        let number = if number_sections && !h.unnumbered && depth <= depth_limit {
            counters[depth - 1] += 1;
            for c in counters.iter_mut().skip(depth) {
                *c = 0;
            }
            for c in counters.iter_mut().take(depth - 1) {
                if *c == 0 {
                    *c = 1;
                }
            }
            let parts: Vec<String> = counters[..depth]
                .iter()
                .enumerate()
                .map(|(k, &c)| {
                    if k == 0 && appendix {
                        letter(c)
                    } else {
                        c.to_string()
                    }
                })
                .collect();
            Some(parts.join("."))
        } else {
            None
        };
        if !h.id.is_empty() {
            cands.push((*line, h.id.clone()));
            if let Some(n) = &number {
                section_numbers.insert(h.id.clone(), n.clone());
            }
        }
        let toc_depth_of = h.level.saturating_sub(base) + 1;
        toc_rows.push(TocRow {
            listed: toc_on && toc_depth_of <= toc_depth && !h.classes.iter().any(|c| c == "unlisted"),
            depth: toc_depth_of,
            id: h.id.clone(),
            number: number.clone(),
            title: h.title.clone(),
        });
        let decorate = h.has_attrs || number.is_some() || shift != 0;
        if h.setext && decorate {
            // The underline goes: the title line becomes an ATX heading.
            refs.skip.insert(*line + 1);
        }
        refs.headings.insert(
            *line,
            Heading {
                raw: lines[*line].to_string(),
                level: (h.level as i64 + shift).clamp(1, 6) as usize,
                title: h.title.clone(),
                id: h.id.clone(),
                decorate,
                setext: h.setext,
                number,
            },
        );
    }

    // Table captions, then figure images and equations.
    let mut taken: HashSet<usize> = HashSet::new();
    for c in 0..lines.len() {
        if !live[c] || taken.contains(&c) {
            continue;
        }
        let Some((id, caption)) = parse_table_caption(lines[c]) else {
            continue;
        };
        let free = |rows: &Range<usize>| !rows.clone().any(|l| taken.contains(&l));
        let rows = table_above(lines, &live, c)
            .filter(&free)
            .or_else(|| table_below(lines, &live, c).filter(&free));
        let Some(rows) = rows else { continue };
        taken.insert(c);
        taken.extend(rows.clone());
        let (key, end) = (c.min(rows.start), c.max(rows.end - 1));
        cands.push((key, id.clone()));
        refs.items.insert(
            key,
            Span {
                item: Item::Table { id, caption, rows },
                end,
            },
        );
    }
    let mut i = parsed.fm.lines;
    while i < lines.len() {
        if !live[i] || taken.contains(&i) {
            i += 1;
            continue;
        }
        if let Some(img) = parse_image(lines[i]) {
            match innermost(i).filter(|&p| prefix_kind(&parsed.divs[p].id) == Some("fig")) {
                Some(p) => subs.push((i, img.id.clone(), p)),
                None => cands.push((i, img.id.clone())),
            }
            refs.items.insert(
                i,
                Span {
                    item: Item::Image(img),
                    end: i,
                },
            );
        } else if let Some((id, tex, end)) = parse_equation(lines, &live, i) {
            cands.push((i, id.clone()));
            refs.items.insert(
                i,
                Span {
                    item: Item::Equation { id, tex },
                    end,
                },
            );
            i = end;
        }
        i += 1;
    }
    // A cell that `include: false` hides is not a target: it has no number
    // and takes none from the cells after it.
    let defaults: Map = match get(meta, "execute") {
        Some(Yaml::Map(m)) => m.clone(),
        _ => Vec::new(),
    };
    cands.extend(
        parsed
            .cells
            .iter()
            .filter(|c| option(&c.options, &defaults, "include") != Some(&Yaml::Bool(false)))
            .map(|c| (c.start, c.label.clone())),
    );

    // Number by kind, in document order.
    let words = &refs.words;
    cands.retain(|(_, id)| words.kind_of(id).is_some());
    cands.sort_by_key(|(line, _)| *line);
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut in_chapter: Option<String> = None;
    for (line, id) in &cands {
        if refs.targets.contains_key(id) {
            continue;
        }
        let kind = refs.words.kind_of(id).unwrap_or_else(|| "fig".into());
        // With chapters the counters start again in each chapter.
        let chapter = refs
            .words
            .chapters()
            .then(|| {
                chapter_marks
                    .iter()
                    .rev()
                    .find(|(at, _)| at <= line)
                    .map(|(_, label)| label.clone())
            })
            .flatten();
        if chapter != in_chapter {
            counts.clear();
            in_chapter = chapter.clone();
        }
        let count = counts.entry(kind.clone()).or_insert(0);
        *count += 1;
        let number = if kind == "sec" {
            section_numbers
                .get(id)
                .cloned()
                .unwrap_or_else(|| count.to_string())
        } else {
            refs.words.number(&kind, *count, chapter.as_deref())
        };
        refs.targets.insert(id.clone(), Target { kind, number });
    }
    subs.sort_by_key(|(line, _, _)| *line);
    // A figure with `layout="[[1,1],[1]]"` places its cells (the subfigures
    // directly in it) in those rows.
    for (n, d) in parsed.divs.iter().enumerate() {
        let Some(spec) = get(&d.attrs, "layout").filter(|_| prefix_kind(&d.id) == Some("fig"))
        else {
            continue;
        };
        let cells: Vec<usize> = subs
            .iter()
            .filter(|(_, _, parent)| *parent == n)
            .map(|(line, _, _)| *line)
            .collect();
        let plans = layout::plan(&layout::flat_text(spec), cells.len());
        for (line, plan) in cells.into_iter().zip(plans) {
            refs.plans.insert(line, plan);
        }
    }
    let mut seen: HashMap<usize, u8> = HashMap::new();
    for (_, id, parent) in subs {
        let slot = seen.entry(parent).or_insert(0);
        *slot += 1;
        let sub_letter = refs.words.number("subref", *slot as usize, None);
        if refs.targets.contains_key(&id) {
            continue;
        }
        let number = refs
            .targets
            .get(&parsed.divs[parent].id)
            .map(|t| t.number.clone())
            .unwrap_or_default();
        refs.targets.insert(
            id.clone(),
            Target {
                kind: "fig".to_string(),
                number: format!("{number}{sub_letter}"),
            },
        );
        refs.letters.insert(id, sub_letter);
    }
    // The contents, once references in headings can be resolved: the anchor
    // of a heading comes from the text the draft will give comrak.
    if toc_on {
        let mut anchorizer = comrak::Anchorizer::new();
        let mut toc_items: Vec<(usize, String, Option<String>, String)> = Vec::new();
        for row in toc_rows {
            let text = heading_text(&row.title, &refs);
            let anchor = anchorizer.anchorize(&slug_text(&text));
            if row.listed {
                let link = if row.id.is_empty() { anchor } else { row.id };
                toc_items.push((row.depth, link, row.number, plain(&text)));
            }
        }
        refs.toc = build_toc(&toc_items, &toc_title);
    }
    refs
}

/// One heading for the contents.
struct TocRow {
    listed: bool,
    depth: usize,
    id: String,
    number: Option<String>,
    title: String,
}

/// A heading's text as the draft gives it to comrak: references resolved,
/// the markup of the HTML they make dropped.
fn heading_text(title: &str, refs: &Refs) -> String {
    let resolved = crossrefs(title, refs, false);
    let mut out = String::with_capacity(resolved.len());
    let mut chars = resolved.chars().peekable();
    while let Some(c) = chars.next() {
        let tag = c == '<' && chars.peek().is_some_and(|n| n.is_ascii_alphabetic() || *n == '/');
        if tag {
            for n in chars.by_ref() {
                if n == '>' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn label_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b':' || b == b'-'
}

/// One reference as written: `@fig-x`, `-@fig-x`, `@Fig-x` or inside
/// brackets `[Fig. @fig-x; @tbl-y]`.
struct Cite {
    /// With the first letter in lower case, as targets are keyed.
    id: String,
    /// Written `@Fig-x`: the prefix takes a capital.
    upper: bool,
    /// Written `-@fig-x`: the number alone.
    bare: bool,
    /// Text before the `@` in brackets: it replaces the prefix.
    prefix: Option<String>,
}

/// The cite that starts with the `@` at `at`, and where it ends.
fn parse_cite(text: &str, at: usize, refs: &Refs) -> Option<(Cite, usize)> {
    let rest = &text[at + 1..];
    let first = rest.chars().next().filter(char::is_ascii_alphabetic)?;
    let lowered = format!("{}{}", first.to_ascii_lowercase(), &rest[1..]);
    let kind = refs.words.kind_of(&lowered)?;
    let label_start = kind.len() + 1;
    let b = lowered.as_bytes();
    let mut end = label_start;
    while end < b.len() && label_char(b[end]) {
        end += 1;
    }
    if end == label_start {
        return None;
    }
    while end + 1 < b.len() && b[end] == b'.' && label_char(b[end + 1]) {
        end += 1;
        while end < b.len() && label_char(b[end]) {
            end += 1;
        }
    }
    let cite = Cite {
        id: lowered[..end].to_string(),
        upper: first.is_ascii_uppercase(),
        bare: false,
        prefix: None,
    };
    Some((cite, at + 1 + end))
}

/// A bracketed group of references, `[@fig-a; Tab. @tbl-b]`, from the `[` at
/// `start`; none unless every part of it is a reference.
fn cite_group(text: &str, start: usize, refs: &Refs) -> Option<(Vec<Cite>, usize)> {
    let close = start + text[start..].find(']')?;
    if matches!(text.as_bytes().get(close + 1), Some(b'(' | b'[' | b'{')) {
        return None;
    }
    let inner = &text[start + 1..close];
    let mut cites = Vec::new();
    let mut offset = start + 1;
    for part in inner.split(';') {
        let at = part.find('@')?;
        let (before, after) = (&part[..at], offset + at);
        let (cite, end) = parse_cite(text, after, refs)?;
        if !text[end..offset + part.len()].trim().is_empty() {
            return None;
        }
        let mut before = before.trim();
        let mut bare = false;
        if let Some(stripped) = before.strip_suffix('-') {
            if stripped.is_empty() || stripped.ends_with(char::is_whitespace) {
                before = stripped.trim_end();
                bare = true;
            }
        }
        cites.push(Cite {
            bare,
            prefix: (!before.is_empty()).then(|| before.to_string()),
            ..cite
        });
        offset += part.len() + 1;
    }
    (!cites.is_empty()).then_some((cites, close + 1))
}

/// The text of a run of references, or none for the Pandoc draft when one
/// does not resolve (Pandoc then reads it as a citation).
fn render_cites(cites: &[Cite], refs: &Refs, pandoc: bool) -> Option<String> {
    let links = refs.words.hyperlinks();
    let mut parts = Vec::new();
    for cite in cites {
        let label = refs.reference_text(&cite.id, cite.bare, cite.prefix.as_deref(), cite.upper);
        parts.push(match label {
            Some(label) if pandoc && links => format!("[{label}](#{})", cite.id),
            Some(label) if pandoc => label,
            Some(label) if links => format!(
                "<a class=\"quarto-crossref crossref\" href=\"#{}\">{}</a>",
                escape(&cite.id),
                escape(&label)
            ),
            Some(label) => escape(&label),
            // Quarto's own mark for a reference it cannot resolve.
            None if !pandoc => format!("**?@{}**", cite.id),
            None => return None,
        });
    }
    Some(parts.join(&format!("{} ", refs.words.ref_delim())))
}

/// `@fig-x` and friends in one stretch of prose: `@fig-x`, `-@fig-x` (the
/// number alone), `@Fig-x` (a capital), `[Fig. @fig-x]` (a prefix of one's
/// own) and `[@fig-a; @fig-b]` (several).
///
/// With `pandoc` a resolved reference is a Markdown link, which Pandoc reads;
/// otherwise it is the preview's HTML, and one that does not resolve is
/// Quarto's bold `?@fig-x`. Inline code is never rewritten.
pub(crate) fn crossrefs(text: &str, refs: &Refs, pandoc: bool) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while let Some(at) = text[i..].find(['@', '`', '[']) {
        let start = i + at;
        i = start + 1;
        match b[start] {
            b'`' => {
                i = code_span_end(text, start).unwrap_or(start + backtick_run(text, start));
            }
            b'[' => {
                if let Some((cites, end)) = cite_group(text, start, refs) {
                    if let Some(html) = render_cites(&cites, refs, pandoc) {
                        out.push_str(&text[copied..start]);
                        out.push_str(&html);
                        copied = end;
                        i = end;
                    }
                }
            }
            _ => {
                let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
                if start > 0 && word(b[start - 1]) {
                    continue;
                }
                let Some((mut cite, end)) = parse_cite(text, start, refs) else {
                    continue;
                };
                // `-@fig-x`, when the dash is not part of a word.
                let mut from = start;
                if start > 0 && b[start - 1] == b'-' && (start == 1 || !word(b[start - 2])) {
                    cite.bare = true;
                    from = start - 1;
                }
                if from >= copied {
                    if let Some(html) = render_cites(&[cite], refs, pandoc) {
                        out.push_str(&text[copied..from]);
                        out.push_str(&html);
                        copied = end;
                    }
                }
                i = end;
            }
        }
    }
    out.push_str(&text[copied..]);
    out
}

// ------------------------------------------------------------------ hooks

/// Whether a div is offered to [`div_open`]: a numbered target, a proof, a
/// remark or solution, or an appendix.
/// Whether the draft keeps this div's fence line as written: Pandoc reads
/// fenced divs itself, and raw HTML would be dropped from its PDF.
/// Whether a div's fences go to the draft as written. Pandoc reads fenced
/// divs itself, and the HTML the preview makes for them (callouts, margins,
/// figures, theorems) would vanish from a PDF, so the Pandoc draft keeps
/// every div as authored.
pub(crate) fn passes_through(_div: &Div, ctx: &Context) -> bool {
    ctx.for_pandoc
}

pub(crate) fn wants(div: &Div, ctx: &Context) -> bool {
    ctx.refs.divs.contains_key(&div.start) || is_appendix(div)
}

fn id_attribute(div: &Div) -> String {
    if div.id.is_empty() {
        String::new()
    } else {
        format!(" id=\"{}\"", escape(&div.id))
    }
}

fn layout_columns(div: &Div) -> Option<usize> {
    match get(&div.attrs, "layout-ncol")? {
        Yaml::Num(n) if *n >= 1.0 => Some(*n as usize),
        Yaml::Str(s) => s.trim().parse().ok().filter(|&n| n >= 1),
        _ => None,
    }
}

/// The opening HTML for a div that is a numbered target, a proof or an
/// appendix, or none to let `divs::open` render it. Must answer together
/// with [`div_close`].
pub(crate) fn div_open(div: &Div, ctx: &mut Context) -> Option<String> {
    let refs = ctx.refs;
    let id = id_attribute(div);
    if is_appendix(div) {
        return Some(format!("<section class=\"appendix\"{id}>"));
    }
    let info = refs.divs.get(&div.start)?;
    match info.kind.as_str() {
        kind if kind == "fig" || refs.is_custom(kind) => {
            let mut classes = vec![if refs.letters.contains_key(&div.id) {
                "subfigure".to_string()
            } else if kind == "fig" {
                "figure".to_string()
            } else {
                format!("figure custom-float float-{kind}")
            }];
            let mut styles: Vec<String> = Vec::new();
            if let Some(cols) = layout_columns(div) {
                classes.push("figure-grid".to_string());
                styles.push(format!("--lp-cols:{cols}"));
            } else if get(&div.attrs, "layout").is_some() {
                classes.push("figure-layout".to_string());
            }
            classes.extend(layout::align_classes(&div.attrs));
            classes.extend(layout::div_columns(&div.classes));
            classes.extend(refs.column_class(&div.attrs, "fig"));
            let mut seen = HashSet::new();
            classes.retain(|c| seen.insert(c.clone()));
            let plan = refs.plans.get(&div.start);
            if let Some(p) = plan {
                styles.push(format!("flex-basis:{}", p.width));
            }
            let style = if styles.is_empty() {
                String::new()
            } else {
                format!(" style=\"{}\"", styles.join(";"))
            };
            let above = if refs.cap_location(&div.attrs, "fig") == CapLocation::Top {
                refs.figcaption_in(&div.id, &info.caption, CapLocation::Top)
            } else {
                String::new()
            };
            let lead = plan.map_or("", |p| p.before.as_str());
            Some(format!(
                "{lead}<figure class=\"{}\"{id}{style}>{above}",
                classes.join(" ")
            ))
        }
        "tbl" => {
            let location = refs.cap_location(&div.attrs, "tbl");
            let mut classes = layout::div_columns(&div.classes);
            classes.extend(refs.column_class(&div.attrs, "tbl"));
            let mut seen = HashSet::new();
            classes.retain(|c| seen.insert(c.clone()));
            Some(refs.table_open_in(&div.id, &info.caption, location, &classes))
        }
        "proof" => {
            let mut html = format!("<div class=\"proof\"{id}>");
            if info.inline_line.is_none() {
                html.push_str(&format!("\n\n<p>{}</p>", refs.div_label(info)));
            }
            Some(html)
        }
        kind => {
            let mut html = format!("<div class=\"theorem theorem-{kind}\"{id}>");
            if info.inline_line.is_none() {
                html.push_str(&format!("\n\n<p>{}</p>", refs.div_label(info)));
            }
            Some(html)
        }
    }
}

/// The closing HTML that pairs with [`div_open`].
pub(crate) fn div_close(div: &Div, ctx: &mut Context) -> Option<String> {
    let refs = ctx.refs;
    if is_appendix(div) {
        return Some("</section>".to_string());
    }
    let info = refs.divs.get(&div.start)?;
    match info.kind.as_str() {
        kind if kind == "fig" || refs.is_custom(kind) => Some(match refs.cap_location(&div.attrs, "fig") {
            CapLocation::Top => "</figure>".to_string(),
            location => format!(
                "{}</figure>",
                refs.figcaption_in(&div.id, &info.caption, location)
            ),
        }),
        "tbl" => Some(refs.table_close(
            &div.id,
            &info.caption,
            refs.cap_location(&div.attrs, "tbl"),
        )),
        _ => Some("</div>".to_string()),
    }
}

/// Whether a line is an ATX heading (`#` to `######` and a space or nothing).
pub(crate) fn is_heading(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return false;
    }
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(char::is_whitespace)
}

/// Whether the line at `index` is a heading the draft rewrites: an ATX one,
/// or the title line of a setext one.
pub(crate) fn is_heading_at(line: &str, index: usize, ctx: &Context) -> bool {
    is_heading(line) || ctx.refs.headings.get(&index).is_some_and(|h| h.setext)
}

/// The replacement for heading line `index` (section number, anchor for its
/// `{#id}`, attributes removed), or none to leave it. Called for each heading
/// line of the main text, before references are resolved in it.
pub(crate) fn decorate_heading(line: &str, index: usize, ctx: &mut Context) -> Option<String> {
    if ctx.for_pandoc {
        return None;
    }
    let h = ctx.refs.headings.get(&index)?;
    if !h.decorate || h.raw != line {
        return None;
    }
    let mut out = "#".repeat(h.level);
    out.push(' ');
    if !h.id.is_empty() {
        out.push_str(&format!("<span id=\"{}\"></span>", escape(&h.id)));
    }
    if let Some(number) = &h.number {
        // The number is an attribute the stylesheet shows, not text: comrak
        // makes a heading's id from its text, so a number there would change
        // the id (`1intro`) and move every anchor when sections renumber.
        out.push_str(&format!(
            "<span class=\"section-number\" data-number=\"{}\"></span>",
            escape(number)
        ));
    }
    out.push_str(&h.title);
    Some(out)
}

/// The table of contents for `toc: true`, placed after the title block.
pub(crate) fn toc(ctx: &Context) -> Option<String> {
    (!ctx.for_pandoc && !ctx.refs.toc.is_empty()).then(|| ctx.refs.toc.clone())
}

/// Line-level constructs: figure images, table captions and their tables,
/// labelled equations, the lines a caption or a name moved out of, and the
/// first line of a theorem or proof, which carries the label. Writes into
/// `out` and answers how many source lines it took, or none to let the
/// driver handle line `i` (it may have written something before it).
pub(crate) fn line(i: usize, lines: &[&str], ctx: &mut Context, out: &mut Out) -> Option<usize> {
    let refs = ctx.refs;
    if ctx.for_pandoc {
        return pandoc_line(i, lines, ctx, out);
    }
    if refs.skip.contains(&i) {
        return Some(1);
    }
    if let Some(span) = refs.items.get(&i) {
        let origin = (i, span.end);
        match &span.item {
            Item::Image(img) => out.block(&refs.image_html(img, refs.plans.get(&i)), origin),
            Item::Equation { id, tex } => out.block(&refs.equation_html(id, tex), origin),
            Item::Table { id, caption, rows } => {
                let none = Map::new();
                let location = refs.cap_location(&none, "tbl");
                let classes: Vec<String> = refs.column_class(&none, "tbl").into_iter().collect();
                out.block(&refs.table_open_in(id, caption, location, &classes), origin);
                super::tables::mark(lines, rows.start, ctx, out);
                for l in rows.clone() {
                    let text = prose(lines[l], ctx);
                    out.push(&text, Some(l), (l, l));
                }
                out.block(&refs.table_close(id, caption, location), origin);
            }
        }
        return Some(span.end + 1 - i);
    }
    let info = refs.divs.get(refs.inline.get(&i)?)?;
    let text = prose(lines[i], ctx);
    out.push(&format!("{} {text}", refs.div_label(info)), Some(i), (i, i));
    Some(1)
}

/// For the draft Pandoc reads, only a labelled equation changes: its math
/// stays and the `{#eq-x}` after it, which Pandoc would print, goes. Figures,
/// tables, captions and theorems are Pandoc-native as written.
fn pandoc_line(i: usize, lines: &[&str], ctx: &mut Context, out: &mut Out) -> Option<usize> {
    let span = ctx.refs.items.get(&i)?;
    let Item::Equation { id, .. } = &span.item else {
        return None;
    };
    // The identifier moves to a fenced div around the math, which Pandoc
    // keeps as the target of `[Equation 1](#eq-x)`.
    out.push(&format!("::: {{#{id}}}"), None, (i, span.end));
    for (l, &line) in lines.iter().enumerate().take(span.end + 1).skip(i) {
        let from = if l == i {
            line.find("$$").map_or(0, |k| k + 2)
        } else {
            0
        };
        let text = if l == span.end {
            line[from..]
                .find("$$")
                .map_or(line, |k| &line[..from + k + 2])
        } else {
            line
        };
        out.push(text, Some(l), (l, l));
    }
    out.push(":::", None, (i, span.end));
    Some(span.end + 1 - i)
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::{compose, no_files};

    #[test]
    fn fig_link_wraps_the_image_and_fig_alt_is_its_alt() {
        let d = compose("![Cap](a.png){#fig-a fig-link=\"https://x.org/a\" fig-alt=\"Alt\"}\n").markdown;
        assert!(
            d.contains("<a href=\"https://x.org/a\"><img src=\"a.png\" alt=\"Alt\"></a>"),
            "{d}"
        );
    }

    #[test]
    fn shift_heading_level_moves_headings_and_clamps() {
        let d = compose("---\nshift-heading-level-by: 1\n---\n\n# One\n\n###### Deep\n").markdown;
        assert!(d.contains("\n## One\n"), "{d}");
        assert!(d.contains("\n###### Deep\n"), "{d}");
    }

    #[test]
    fn number_offset_starts_the_counters_and_implies_numbering() {
        let d = compose("---\nnumber-offset: [5, 2]\n---\n\n# One\n\n## Sub\n").markdown;
        assert!(d.contains("data-number=\"6\""), "{d}");
        assert!(d.contains("data-number=\"6.1\""), "{d}");
        let d = compose("---\nnumber-offset: 5\n---\n\n# One\n").markdown;
        assert!(d.contains("data-number=\"6\""), "{d}");
    }

    #[test]
    fn unlisted_headings_stay_out_of_the_contents() {
        let d = compose("---\ntoc: true\n---\n\n# One\n\n# Two {.unlisted}\n").markdown;
        assert!(d.contains("href=\"#one\""), "{d}");
        assert!(!d.contains("href=\"#two\""), "{d}");
    }

    fn assert_has(draft: &str, expected: &str) {
        assert!(
            draft.contains(expected),
            "missing {expected:?} in:\n{draft}"
        );
    }

    #[test]
    fn a_pandoc_equation_keeps_its_target_in_a_fenced_div() {
        let d = crate::quarto::draft(
            "$$ x $$ {#eq-x}\n\nSee @eq-x.\n",
            "main.qmd",
            &|_: &str| None,
            &crate::quarto::Extras::default(),
            true,
        )
        .markdown;
        assert_has(&d, "::: {#eq-x}\n$$ x $$\n:::");
        assert_has(&d, "[Equation 1](#eq-x)");
    }

    #[test]
    fn cross_references_are_numbered_in_document_order() {
        let source = "::: {.callout-note #intro}\nSee @fig-trend.\n:::\n\n\
                      ```{r #fig-trend}\n#| fig-cap: Trend\nplot(1)\n```\n\n\
                      See @fig-trend and @tbl-one and @fig-nope, `@fig-trend` and [@fig-trend](x).\n\n\
                      ::: {#tbl-one}\nTable caption\n:::\n";
        let d = compose(source).markdown;
        assert_has(&d, "<div class=\"callout callout-note\" id=\"intro\">");
        assert_has(&d, "<figure class=\"table\" id=\"tbl-one\">");
        assert_has(
            &d,
            "See <a class=\"quarto-crossref crossref\" href=\"#fig-trend\">Figure 1</a>.\n",
        );
        assert_has(
            &d,
            "<a class=\"quarto-crossref crossref\" href=\"#fig-trend\">Figure 1</a> and ",
        );
        assert_has(&d, "href=\"#tbl-one\">Table 1</a>");
        assert_has(&d, "@fig-nope");
        assert_has(&d, "`@fig-trend`");
        assert_has(&d, "[@fig-trend](x)");
    }

    #[test]
    fn a_figure_div_takes_its_caption_from_the_last_paragraph() {
        let d = compose("::: {#fig-a}\n![](a.png)\n\nA *nice* plot\n:::\n\nSee @fig-a.\n").markdown;
        assert_has(&d, "<figure class=\"figure\" id=\"fig-a\">");
        assert_has(&d, "![](a.png)");
        assert_has(
            &d,
            "<figcaption><span class=\"caption-label\">Figure 1.</span> A <em>nice</em> plot</figcaption></figure>",
        );
        assert!(!d.contains("A *nice* plot"), "the caption moved: {d}");
        assert_has(&d, "href=\"#fig-a\">Figure 1</a>");
    }

    #[test]
    fn a_layout_div_is_a_grid_of_subfigures() {
        let source = "::: {#fig-g layout-ncol=2}\n![A](a.png){#fig-ga}\n![B](b.png){#fig-gb}\n\nGrid caption\n:::\n\nSee @fig-gb.\n";
        let d = compose(source).markdown;
        assert_has(
            &d,
            "<figure class=\"figure figure-grid\" id=\"fig-g\" style=\"--lp-cols:2\">",
        );
        assert_has(
            &d,
            "<figure class=\"subfigure\" id=\"fig-ga\"><img src=\"a.png\" alt=\"A\"><figcaption><span class=\"caption-label\">(a)</span> A</figcaption></figure>",
        );
        assert_has(&d, "<span class=\"caption-label\">(b)</span> B");
        assert_has(
            &d,
            "<span class=\"caption-label\">Figure 1.</span> Grid caption",
        );
        assert_has(&d, "href=\"#fig-gb\">Figure 1b</a>");
    }

    #[test]
    fn a_figure_image_keeps_its_source_and_takes_its_size() {
        let d =
            compose("![Cap](p.png){#fig-i width=50% fig-align=center}\n\nSee @fig-i.\n").markdown;
        assert_has(
            &d,
            "<figure class=\"figure\" id=\"fig-i\"><img src=\"p.png\" alt=\"Cap\" style=\"width:50%;display:block;margin-left:auto;margin-right:auto\"><figcaption><span class=\"caption-label\">Figure 1.</span> Cap</figcaption></figure>",
        );
        assert_has(&d, "href=\"#fig-i\">Figure 1</a>");
    }

    #[test]
    fn a_table_caption_before_or_after_the_table_makes_a_table_figure() {
        let after =
            compose("| a | b |\n|---|---|\n| 1 | 2 |\n\n: Results {#tbl-r}\n\nSee @tbl-r.\n")
                .markdown;
        assert_has(
            &after,
            "<figure class=\"table\" id=\"tbl-r\">\n<figcaption><span class=\"caption-label\">Table 1.</span> Results</figcaption>",
        );
        assert!(after.find("<figure").unwrap() < after.find("| a | b |").unwrap());
        assert!(after.find("| 1 | 2 |").unwrap() < after.find("</figure>").unwrap());
        assert!(!after.contains(": Results"), "{after}");
        assert_has(&after, "href=\"#tbl-r\">Table 1</a>");
        let before = compose(": Cap {#tbl-b}\n\n| a |\n|---|\n| 1 |\n").markdown;
        assert_has(&before, "<span class=\"caption-label\">Table 1.</span> Cap");
        assert!(before.find("<figure").unwrap() < before.find("| a |").unwrap());
        assert!(before.find("| 1 |").unwrap() < before.find("</figure>").unwrap());
    }

    #[test]
    fn a_labelled_equation_is_numbered() {
        let source = "$$ E = mc^2 $$ {#eq-e}\n\n$$\na &= b \\\\\nc &= d\n$$ {#eq-m}\n\nSee @eq-e and @eq-m.\n";
        let d = compose(source).markdown;
        assert_has(
            &d,
            "<div class=\"equation\" id=\"eq-e\"><span data-math-style=\"display\">E = mc^2</span><span class=\"equation-number\">(1)</span></div>",
        );
        assert_has(
            &d,
            "<span data-math-style=\"display\">a &amp;= b \\\\\nc &amp;= d</span><span class=\"equation-number\">(2)</span>",
        );
        assert_has(&d, "href=\"#eq-m\">Equation 2</a>");
        assert_has(&d, "href=\"#eq-e\">Equation 1</a>");
    }

    #[test]
    fn theorems_are_labelled_per_kind_and_proofs_are_not_numbered() {
        let source = "::: {#thm-main name=\"Main result\"}\nEvery x is y.\n:::\n\n\
                      ::: {#lem-aux}\n## Helper\nAnother.\n:::\n\n\
                      ::: {#thm-two}\nSecond.\n:::\n\n\
                      ::: proof\nTrivial.\n:::\n\n\
                      ::: {#def-x}\n- item\n:::\n\nSee @thm-two and @lem-aux.\n";
        let d = compose(source).markdown;
        assert_has(&d, "<div class=\"theorem theorem-thm\" id=\"thm-main\">");
        assert_has(
            &d,
            "<span class=\"theorem-label\">Theorem 1</span> <span class=\"theorem-name\">(Main result)</span>. Every x is y.",
        );
        assert_has(
            &d,
            "<span class=\"theorem-label\">Lemma 1</span> <span class=\"theorem-name\">(Helper)</span>. Another.",
        );
        assert!(!d.contains("## Helper"), "{d}");
        assert_has(
            &d,
            "<span class=\"theorem-label\">Theorem 2</span>. Second.",
        );
        assert_has(&d, "<div class=\"proof\">");
        assert_has(&d, "<span class=\"proof-label\">Proof.</span> Trivial.");
        assert_has(
            &d,
            "<p><span class=\"theorem-label\">Definition 1</span>.</p>",
        );
        assert_has(&d, "href=\"#thm-two\">Theorem 2</a>");
        assert_has(&d, "href=\"#lem-aux\">Lemma 1</a>");
    }

    #[test]
    fn sections_are_numbered_and_anchored() {
        let source = "---\ntitle: T\nnumber-sections: true\n---\n\n# Intro {#sec-intro}\n\n## Sub\n\n# Methods\n\n## Detail {.unnumbered}\n\n# Last {#sec-last}\n\nSee @sec-intro and @sec-last.\n";
        let d = compose(source).markdown;
        assert_has(
            &d,
            "\n# <span id=\"sec-intro\"></span><span class=\"section-number\" data-number=\"1\"></span>Intro\n",
        );
        assert_has(
            &d,
            "\n## <span class=\"section-number\" data-number=\"1.1\"></span>Sub\n",
        );
        assert_has(
            &d,
            "\n# <span class=\"section-number\" data-number=\"2\"></span>Methods\n",
        );
        assert_has(&d, "\n## Detail\n");
        assert_has(
            &d,
            "\n# <span id=\"sec-last\"></span><span class=\"section-number\" data-number=\"3\"></span>Last\n",
        );
        assert_has(&d, "href=\"#sec-intro\">Section 1</a>");
        assert_has(&d, "href=\"#sec-last\">Section 3</a>");
        let html = crate::markdown::render_body(&d);
        assert_has(&html, "id=\"intro\"");
        assert_has(&html, "id=\"sec-intro\"");
    }

    #[test]
    fn callout_titles_and_tabs_are_not_sections() {
        let source = "---\ntitle: T\ntoc: true\nnumber-sections: true\n---\n\n# One\n\n::: {.callout-note}\n## Note title\nBody.\n:::\n\n::: {.panel-tabset}\n## R\nr\n## Py\npy\n:::\n\n# Two\n";
        let d = compose(source).markdown;
        assert!(
            !d.contains("Note title</a>") && !d.contains(">R</a>"),
            "{d}"
        );
        assert_has(&d, "data-number=\"2\"></span>Two");
    }

    #[test]
    fn appendix_headings_are_lettered() {
        let source = "---\ntitle: T\nnumber-sections: true\n---\n\n# One\n\n# Extra {.appendix}\n\n## Sub\n\n# More\n";
        let d = compose(source).markdown;
        assert_has(
            &d,
            "\n# <span class=\"section-number\" data-number=\"1\"></span>One\n",
        );
        assert_has(
            &d,
            "\n# <span class=\"section-number\" data-number=\"A\"></span>Extra\n",
        );
        assert_has(
            &d,
            "\n## <span class=\"section-number\" data-number=\"A.1\"></span>Sub\n",
        );
        assert_has(
            &d,
            "\n# <span class=\"section-number\" data-number=\"B\"></span>More\n",
        );
        let wrapped = compose(
            "---\ntitle: T\nnumber-sections: true\n---\n\n# One\n\n::: {.appendix}\n# Extra\n:::\n",
        )
        .markdown;
        assert_has(&wrapped, "<section class=\"appendix\">");
        assert_has(&wrapped, "</section>");
        assert_has(
            &wrapped,
            "<span class=\"section-number\" data-number=\"A\"></span>Extra",
        );
    }

    #[test]
    fn the_contents_follow_the_title_block() {
        let source =
            "---\ntitle: T\ntoc: true\ntoc-title: Inhalt\n---\n\n# One\n\n## Two\n\n# Three\n";
        let d = compose(source).markdown;
        assert_has(
            &d,
            "<nav class=\"toc\"><p class=\"toc-title\">Inhalt</p><ol><li><a href=\"#one\">One</a><ol><li><a href=\"#two\">Two</a></li></ol></li><li><a href=\"#three\">Three</a></li></ol></nav>",
        );
        assert!(d.find("</header>").unwrap() < d.find("<nav").unwrap());
        let numbered = compose(
            "---\ntitle: T\ntoc: true\ntoc-depth: 1\nnumber-sections: true\n---\n\n# One {#sec-one}\n\n## Two\n",
        )
        .markdown;
        assert_has(
            &numbered,
            "<ol><li><a href=\"#sec-one\"><span class=\"section-number\">1</span> One</a></li></ol>",
        );
        assert!(!numbered.contains("Two</a>"), "{numbered}");
    }

    #[test]
    fn crossref_words_come_from_the_front_matter() {
        let source = "---\ntitle: T\ncrossref:\n  fig-prefix: Fig.\n  fig-title: Abb\n---\n\n![C](x.png){#fig-a}\n\nSee @fig-a.\n";
        let d = compose(source).markdown;
        assert_has(&d, "<span class=\"caption-label\">Abb 1.</span> C");
        assert_has(&d, "href=\"#fig-a\">Fig. 1</a>");
    }

    #[test]
    fn a_figure_div_folds_into_one_block_with_its_caption() {
        let blocks = crate::blocks::compile_blocks(
            "::: {#fig-a}\n![](a.png)\n\nA nice plot\n:::\n",
            "p.qmd",
            &no_files,
            &crate::markdown::no_assets,
        );
        let value: serde_json::Value = serde_json::from_str(&blocks.json).unwrap();
        let list = value["blocks"].as_array().unwrap();
        assert_eq!(list.len(), 1, "{}", blocks.json);
        let html = list[0]["html"].as_str().unwrap();
        assert!(html.contains("<img src=\"a.png\""), "{html}");
        assert!(
            html.contains("<span class=\"caption-label\">Figure 1.</span> A nice plot"),
            "{html}"
        );
        assert_eq!(
            (list[0]["from_line"].as_u64(), list[0]["to_line"].as_u64()),
            (Some(1), Some(5))
        );
    }

    fn pandoc(source: &str) -> String {
        crate::quarto::draft(
            source,
            "main.qmd",
            &no_files,
            &crate::quarto::Extras::default(),
            true,
        )
        .markdown
    }

    const EVERYTHING: &str = "---\ntitle: T\ntoc: true\nnumber-sections: true\n---\n\n\
        # Intro {#sec-intro}\n\n\
        ![Cap](p.png){#fig-x width=50%}\n\n\
        | a | b |\n|---|---|\n| 1 | 2 |\n\n: Cap {#tbl-t}\n\n\
        $$ y = x $$ {#eq-e}\n\n\
        $$\ny = z\n$$ {#eq-f}\n\n\
        ::: {#fig-d}\n![](d.png)\n\nDiv caption\n:::\n\n\
        ::: {#thm-a}\n**Name.** Statement.\n:::\n\n\
        See @fig-x, @tbl-t, @eq-e and @sec-intro.\n";

    #[test]
    fn pandoc_gets_native_markdown_for_every_reference_feature() {
        let d = pandoc(EVERYTHING);
        for banned in [
            "<figure",
            "<div class=\"equation\"",
            "<nav",
            "data-number",
            "<a ",
            "<span",
        ] {
            assert!(!d.contains(banned), "{banned} in:\n{d}");
        }
        for kept in [
            "$$ y = x $$",
            "$$\ny = z\n$$\n",
            "![Cap](p.png){#fig-x width=50%}",
            ": Cap {#tbl-t}",
            "# Intro {#sec-intro}",
            "::: {#fig-d}",
            "Div caption",
            "::: {#thm-a}",
            "[Figure 1](#fig-x)",
            "[Table 1](#tbl-t)",
            "[Equation 1](#eq-e)",
            "[Section 1](#sec-intro)",
        ] {
            assert_has(&d, kept);
        }
        // The attribute text is gone; each equation's id is on a fenced div.
        assert!(!d.contains("$$ {#eq-"), "{d}");
        let equations = d.matches("::: {#eq-").count();
        assert_eq!(equations, 2, "{d}");
        assert_eq!(d.matches("\n:::\n").count(), 2 + equations, "{d}");
    }

    #[test]
    fn the_pandoc_draft_keeps_callouts_margins_and_tabsets_as_fenced_divs() {
        let source = "::: {.callout-note}\n## Title\nBody.\n:::\n\n::: {.column-margin}\nAside.\n:::\n\n::: {.panel-tabset}\n## R\nr\n:::\n";
        let d = crate::quarto::draft(
            source,
            "main.qmd",
            &|_: &str| None,
            &crate::quarto::Extras::default(),
            true,
        )
        .markdown;
        assert!(
            d.contains("::: {.callout-note}") && d.contains("## Title"),
            "{d}"
        );
        assert!(
            d.contains("::: {.column-margin}") && d.contains("## R"),
            "{d}"
        );
        assert!(
            !d.contains("<div") && !d.contains("<aside") && !d.contains("<details"),
            "{d}"
        );
    }

    #[test]
    fn the_preview_keeps_its_html_for_the_same_document() {
        let d = compose(EVERYTHING).markdown;
        for html in [
            "<figure",
            "<div class=\"equation\"",
            "<nav",
            "data-number",
            "<a ",
        ] {
            assert_has(&d, html);
        }
    }

    #[test]
    fn heading_ids_decode_entities_like_comrak() {
        let source = "---\ntoc: true\n---\n\n## Cats &amp; dogs\n\n## A &lt;b&gt; &quot;q&quot; &#39;s&#39; &nbsp;x&#65;&#x42;\n\n\
                      ## **Bold** and _em_ with `co&amp;de`\n";
        let d = compose(source).markdown;
        assert_has(&d, "href=\"#cats--dogs\"");
        assert_has(&d, "Cats &amp; dogs</a>");
        assert_has(&d, "href=\"#a-b-q-s-xab\"");
        assert_has(&d, "href=\"#bold-and-em-with-coampde\"");
    }

    #[test]
    fn inline_code_ends_at_a_run_of_the_same_length() {
        let source = "# H {#sec-h}\n\n``@fig-x`` and `a`` b` and ``a ` @fig-x`` then @sec-h.\n";
        let d = compose(source).markdown;
        assert_has(&d, "``@fig-x`` and `a`` b` and ``a ` @fig-x`` then <a");
        assert_has(&d, "href=\"#sec-h\">Section 1</a>");
        let source = "![C](p.png){#fig-x}\n\n``@fig-x`` and then @fig-x.\n";
        let d = compose(source).markdown;
        assert_has(&d, "``@fig-x`` and then <a class=\"quarto-crossref crossref\" href=\"#fig-x\">Figure 1</a>");
    }

    #[test]
    fn hidden_cells_are_not_numbered_targets() {
        let source = "```{r}\n#| label: fig-hidden\n#| include: false\n1\n```\n\n\
                      ```{r}\n#| label: fig-shown\n#| fig-cap: Shown\n2\n```\n\n\
                      See @fig-hidden and @fig-shown.\n";
        let d = compose(source).markdown;
        assert_has(&d, "See **?@fig-hidden** and <a");
        assert_has(&d, "href=\"#fig-shown\">Figure 1</a>");
        let source = format!("---\nexecute:\n  include: false\n---\n\n{source}");
        let d = compose(&source).markdown;
        assert_has(&d, "See **?@fig-hidden** and **?@fig-shown**.");
        let source = "---\nexecute:\n  include: false\n---\n\n\
                      ```{r}\n#| label: fig-back\n#| include: true\n#| fig-cap: Back\n1\n```\n\nSee @fig-back.\n";
        assert_has(&compose(source).markdown, "href=\"#fig-back\">Figure 1</a>");
    }

    fn link(id: &str, text: &str) -> String {
        format!("<a class=\"quarto-crossref crossref\" href=\"#{id}\">{text}</a>")
    }

    const TWO_FIGURES: &str = "![A](a.png){#fig-a}\n\n![B](b.png){#fig-b}\n\n";

    #[test]
    fn a_dash_gives_the_number_alone() {
        let d = compose(&format!("{TWO_FIGURES}See [-@fig-a] and -@fig-b.\n")).markdown;
        assert_has(&d, &format!("See {} and {}.", link("fig-a", "1"), link("fig-b", "2")));
    }

    #[test]
    fn a_prefix_before_the_reference_replaces_the_word() {
        let d = compose(&format!("{TWO_FIGURES}See [Fig. @fig-b].\n")).markdown;
        assert_has(&d, &format!("See {}.", link("fig-b", "Fig. 2")));
    }

    #[test]
    fn several_references_are_joined_with_the_delimiter() {
        let d = compose(&format!("{TWO_FIGURES}See [@fig-a; @fig-b].\n")).markdown;
        assert_has(
            &d,
            &format!("See {}, {}.", link("fig-a", "Figure 1"), link("fig-b", "Figure 2")),
        );
        let d = compose(&format!(
            "---\ncrossref:\n  ref-delim: ' and'\n---\n\n{TWO_FIGURES}See [@fig-a; @fig-b].\n"
        ))
        .markdown;
        assert_has(&d, &format!("{} and {}", link("fig-a", "Figure 1"), link("fig-b", "Figure 2")));
        // A group that is not all references is left to the citations.
        let d = compose(&format!("{TWO_FIGURES}See [@fig-a; @smith].\n")).markdown;
        assert_has(&d, "; @smith].");
    }

    #[test]
    fn a_capital_makes_the_prefix_a_capital() {
        let source = "---\ncrossref:\n  fig-prefix: fig.\n---\n\n![A](a.png){#fig-a}\n\nSee @fig-a and @Fig-a.\n";
        let d = compose(source).markdown;
        assert_has(&d, &format!("See {} and {}.", link("fig-a", "fig. 1"), link("fig-a", "Fig. 1")));
    }

    #[test]
    fn ref_hyperlink_false_leaves_the_text_alone() {
        let source = "---\ncrossref:\n  ref-hyperlink: false\n---\n\n![A](a.png){#fig-a}\n\nSee @fig-a.\n";
        let d = compose(source).markdown;
        assert_has(&d, "See Figure 1.\n");
        assert!(!d.contains("href=\"#fig-a\">Figure"), "{d}");
    }

    #[test]
    fn an_unresolved_reference_is_a_bold_mark() {
        let d = compose("See @fig-nope and [@tbl-nope].\n").markdown;
        assert_has(&d, "See **?@fig-nope** and **?@tbl-nope**.");
        // Not a reference: an email address, an ordinary citation.
        let d = compose("Write a@fig-x.org or cite @smith.\n").markdown;
        assert_has(&d, "a@fig-x.org or cite @smith.");
    }

    #[test]
    fn labels_set_the_numbering_style() {
        let source = "---\ncrossref:\n  fig-labels: alpha A\n  tbl-labels: roman i\n  labels: roman\n---\n\n\
                      ![A](a.png){#fig-a}\n\n![B](b.png){#fig-b}\n\n$$ x $$ {#eq-x}\n\n$$ y $$ {#eq-y}\n\n\
                      : T {#tbl-t}\n\n| a |\n|---|\n| 1 |\n\nSee @fig-b, @eq-y and @tbl-t.\n";
        let d = compose(source).markdown;
        assert_has(&d, "href=\"#fig-b\">Figure B</a>");
        assert_has(&d, "href=\"#eq-y\">Equation II</a>");
        assert_has(&d, "href=\"#tbl-t\">Table i</a>");
        assert_has(&d, "<span class=\"caption-label\">Figure B.</span>");
    }

    #[test]
    fn subref_labels_number_the_subfigures() {
        let source = "---\ncrossref:\n  subref-labels: roman i\n---\n\n::: {#fig-g layout-ncol=2}\n![A](a.png){#fig-ga}\n![B](b.png){#fig-gb}\n\nGrid\n:::\n\nSee @fig-gb.\n";
        let d = compose(source).markdown;
        assert_has(&d, "<span class=\"caption-label\">(ii)</span> B");
        assert_has(&d, "href=\"#fig-gb\">Figure 1ii</a>");
    }

    #[test]
    fn chapters_prefix_numbers_with_the_chapter() {
        let source = "---\ncrossref:\n  chapters: true\nnumber-sections: true\n---\n\n\
                      # One\n\n![A](a.png){#fig-a}\n\n![B](b.png){#fig-b}\n\n\
                      # Two\n\n![C](c.png){#fig-c}\n\n\
                      # Extra {.appendix}\n\n![D](d.png){#fig-d}\n\n\
                      See @fig-a, @fig-b, @fig-c and @fig-d.\n";
        let d = compose(source).markdown;
        assert_has(&d, "href=\"#fig-a\">Figure 1.1</a>");
        assert_has(&d, "href=\"#fig-b\">Figure 1.2</a>");
        assert_has(&d, "href=\"#fig-c\">Figure 2.1</a>");
        assert_has(&d, "href=\"#fig-d\">Figure A.1</a>");
        assert_has(&d, "<span class=\"caption-label\">Figure 2.1.</span>");
        let off = compose(&source.replace("  chapters: true\n", "  chapters: false\n")).markdown;
        assert_has(&off, "href=\"#fig-c\">Figure 3</a>");
        assert_has(&off, "href=\"#fig-d\">Figure 4</a>");
    }

    #[test]
    fn numbered_callouts_take_a_label_and_can_be_referred_to() {
        let source = "::: {#nte-a .callout-note}\nPlain.\n:::\n\n\
                      ::: {#nte-b .callout-note title=\"Named\"}\nBody.\n:::\n\n\
                      ::: {#tip-a .callout-tip}\nTip.\n:::\n\nSee @nte-b and @tip-a.\n";
        let d = compose(source).markdown;
        assert_has(&d, "<div class=\"callout-title\">Note 1</div>");
        assert_has(&d, "<div class=\"callout-title\">Note 2. Named</div>");
        assert_has(&d, "<div class=\"callout-title\">Tip 1</div>");
        assert_has(&d, "id=\"nte-b\"");
        assert_has(&d, &format!("See {} and {}.", link("nte-b", "Note 2"), link("tip-a", "Tip 1")));
        // Without an id a callout stays as it was.
        let d = compose("::: {.callout-note}\nx\n:::\n").markdown;
        assert_has(&d, "<div class=\"callout-title\">Note</div>");
    }

    #[test]
    fn a_custom_float_is_numbered_like_a_figure() {
        let source = "---\ncrossref:\n  custom:\n    - kind: float\n      key: vid\n      reference-prefix: Video\n      caption-prefix: Clip\n---\n\n\
                      ::: {#vid-a}\n<video src=\"a.mp4\"></video>\n\nThe first\n:::\n\n::: {#vid-b}\nx\n\nThe second\n:::\n\nSee @vid-b.\n";
        let d = compose(source).markdown;
        assert_has(&d, "<figure class=\"figure custom-float float-vid\" id=\"vid-a\">");
        assert_has(&d, "<span class=\"caption-label\">Clip 2.</span> The second");
        assert_has(&d, &format!("See {}.", link("vid-b", "Video 2")));
        let flow = "---\ncrossref:\n  custom: [{kind: float, key: vid, reference-prefix: Video}]\n---\n\n::: {#vid-a}\nx\n\nCap\n:::\n\nSee @vid-a.\n";
        let d = compose(flow).markdown;
        assert_has(&d, "<span class=\"caption-label\">Video 1.</span> Cap");
        assert_has(&d, &format!("See {}.", link("vid-a", "Video 1")));
    }

    #[test]
    fn contents_anchors_follow_the_text_the_draft_gives_comrak() {
        // A reference in a heading becomes its words; an unresolved one is
        // bold text. The anchors in the contents match the heading ids.
        let source = "---\ntoc: true\n---\n\n![A](a.png){#fig-a}\n\n# About @fig-a\n\n# Where is @fig-no\n\n# About Figure 1\n";
        let d = compose(source).markdown;
        assert_has(&d, "<a href=\"#about-figure-1\">About Figure 1</a>");
        assert_has(&d, "<a href=\"#where-is-fig-no\">Where is ?@fig-no</a>");
        // The third heading has the text of the first: comrak numbers it.
        assert_has(&d, "<a href=\"#about-figure-1-1\">About Figure 1</a>");
    }

    #[test]
    fn setext_headings_are_in_the_contents_and_take_their_number() {
        let source = "---\ntoc: true\nnumber-sections: true\n---\n\nFirst part\n==========\n\nBody.\n\nSecond part\n-----------\n\nMore.\n\n# Third\n";
        let d = compose(source).markdown;
        assert_has(&d, "<a href=\"#first-part\"><span class=\"section-number\">1</span> First part</a>");
        assert_has(&d, "<a href=\"#second-part\"><span class=\"section-number\">1.1</span> Second part</a>");
        assert_has(&d, "\n# <span class=\"section-number\" data-number=\"1\"></span>First part\n");
        assert!(!d.contains("==========") && !d.contains("-----------"), "{d}");
        // Without numbers a setext heading is left as written.
        let d = compose("---\ntoc: true\n---\n\nTitle\n=====\n\n# Title\n").markdown;
        assert_has(&d, "Title\n=====\n");
        assert_has(&d, "<a href=\"#title\">Title</a>");
        assert_has(&d, "<a href=\"#title-1\">Title</a>");
    }

    #[test]
    fn caption_location_moves_the_caption_of_figures_and_tables() {
        // A figure image: top by attribute, margin by document option.
        let d = compose("![Cap](p.png){#fig-a cap-location=top}\n").markdown;
        assert_has(&d, "id=\"fig-a\"><figcaption><span class=\"caption-label\">Figure 1.</span> Cap</figcaption><img");
        let d = compose("---\nfig-cap-location: margin\n---\n\n![Cap](p.png){#fig-a}\n").markdown;
        assert_has(&d, "><img src=\"p.png\" alt=\"Cap\"><figcaption class=\"margin-caption\">");
        // A figure div: caption on top is written at the opening fence.
        let d = compose("::: {#fig-d cap-location=top}\n![](d.png)\n\nDiv caption\n:::\n").markdown;
        assert!(
            d.find("<figcaption").unwrap() < d.find("d.png").unwrap(),
            "{d}"
        );
        // A table keeps its caption on top, and moves it below on request.
        let table = "| a |\n|---|\n| 1 |\n\n: Results {#tbl-r}\n";
        let d = compose(table).markdown;
        assert!(d.find("<figcaption").unwrap() < d.find("| a |").unwrap(), "{d}");
        let d = compose(&format!("---\ntbl-cap-location: bottom\n---\n\n{table}")).markdown;
        assert!(d.find("<figcaption").unwrap() > d.find("| a |").unwrap(), "{d}");
        let d = compose("::: {#tbl-x cap-location=margin}\n| a |\n|---|\n| 1 |\n\nCap\n:::\n").markdown;
        assert_has(&d, "<figcaption class=\"margin-caption\">");
    }

    #[test]
    fn wide_column_classes_stay_on_figures_divs_and_come_from_options() {
        let d = compose("::: {.column-page}\nWide\n:::\n").markdown;
        assert_has(&d, "<div class=\"column-page\">");
        let d = compose("::: {#fig-w .column-screen-inset-shaded}\n![](w.png)\n\nCap\n:::\n").markdown;
        assert_has(&d, "<figure class=\"figure column-screen-inset-shaded\" id=\"fig-w\">");
        let d = compose("---\nfig-column: page-right\n---\n\n![Cap](p.png){#fig-a}\n").markdown;
        assert_has(&d, "<figure class=\"figure column-page-right\" id=\"fig-a\">");
        let d = compose("![Cap](p.png){#fig-a fig-column=body-outset}\n").markdown;
        assert_has(&d, "class=\"figure column-body-outset\"");
    }

    #[test]
    fn layout_rows_place_cells_with_spacers_and_alignment() {
        let source = "::: {#fig-g layout=\"[[1,-1,1],[1]]\" layout-valign=center layout-align=right}\n![A](a.png){#fig-ga}\n![B](b.png){#fig-gb}\n![C](c.png){#fig-gc}\n\nCap\n:::\n";
        let d = compose(source).markdown;
        assert_has(
            &d,
            "<figure class=\"figure figure-layout quarto-layout-align-right quarto-layout-valign-center\" id=\"fig-g\">",
        );
        assert_has(&d, "<figure class=\"subfigure\" id=\"fig-ga\" style=\"flex-basis:33.33%\">");
        assert_has(
            &d,
            "<div class=\"quarto-figure-spacer\" style=\"flex-basis:33.33%\"></div><figure class=\"subfigure\" id=\"fig-gb\"",
        );
        assert_has(
            &d,
            "<div class=\"quarto-layout-break\"></div><figure class=\"subfigure\" id=\"fig-gc\" style=\"flex-basis:100%\">",
        );
        assert_has(&d, "<span class=\"caption-label\">(c)</span> C");
    }
}

