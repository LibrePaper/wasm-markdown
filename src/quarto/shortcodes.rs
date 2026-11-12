//! Shortcodes: `{{< name args >}}`.
//!
//! Owner: g7cv. [`expand`] is the extension point: one match arm per
//! shortcode name, `include` today. A name with no arm (and no entry in
//! [`handled`]) is the default: the whole line stays as verbatim source.

use crate::page::escape;

use super::cells::option;
use super::front_matter::{get, set, Yaml};
use super::front_matter::parse_front_matter;
use super::{fence_closes, fence_open, parse, Context, Out};

/// How deep includes may nest before the draft calls it a cycle.
const MAX_INCLUDE_DEPTH: usize = 8;

/// Where a line being expanded comes from.
pub(crate) struct Site<'a> {
    /// How many includes enclose it.
    pub depth: usize,
    /// The normalized names of the files being included, outermost first.
    pub seen: &'a [String],
    /// The file the line is in.
    pub source_name: &'a str,
    /// Its 1-based line there; 0 inside an included file.
    pub line: usize,
}

/// One `{{< name args >}}` in a line.
struct Call<'a> {
    start: usize,
    end: usize,
    name: &'a str,
    /// Everything between the name and `>}}`, spacing included.
    args: &'a str,
}

fn name_length(rest: &str) -> usize {
    rest.char_indices()
        .take_while(|&(n, c)| {
            if n == 0 {
                c.is_ascii_alphabetic()
            } else {
                c.is_ascii_alphanumeric() || c == '_' || c == '-'
            }
        })
        .count()
}

/// The first shortcode that starts at or after `from`.
fn find_shortcode(s: &str, from: usize) -> Option<Call<'_>> {
    let mut pos = from;
    while let Some(k) = s[pos..].find("{{<") {
        let start = pos + k;
        pos = start + 3;
        let rest = s[pos..].trim_start();
        let name_at = s.len() - rest.len();
        let len = name_length(rest);
        if len == 0 {
            continue;
        }
        let after_at = name_at + len;
        if let Some(g) = s[after_at..].find(">}}") {
            return Some(Call {
                start,
                end: after_at + g + 3,
                name: &s[name_at..after_at],
                args: &s[after_at..after_at + g],
            });
        }
    }
    None
}

/// Whether the driver expands a shortcode of this name; a line with any other
/// shortcode is kept whole. Add a name here with its arm in [`expand`].
fn handled(name: &str) -> bool {
    name.eq_ignore_ascii_case("include")
        || matches!(
            name,
            "pagebreak" | "kbd" | "video" | "meta" | "var" | "lipsum" | "placeholder" | "env"
        )
}

/// Whether the `{{<` at `at` is the inside of an escaped `{{{< x >}}}`.
fn escaped(s: &str, at: usize) -> bool {
    at > 0 && s.as_bytes()[at - 1] == b'{'
}

/// Whether a line holds a shortcode other than the handled ones.
pub(crate) fn has_unsupported_shortcode(line: &str) -> bool {
    let mut pos = 0;
    while let Some(k) = line[pos..].find("{{<") {
        let at = pos + k;
        pos += k + 3;
        if escaped(line, at) {
            continue;
        }
        let rest = line[pos..].trim_start();
        let name_len = name_length(rest);
        if name_len == 0 {
            continue;
        }
        let after = &rest[name_len..];
        if let Some(g) = after.find('>') {
            if after[g + 1..].starts_with("}}") && !handled(&rest[..name_len]) {
                return true;
            }
        }
    }
    false
}

/// The text a shortcode stands for, or none to leave it as written.
pub(crate) fn expand(name: &str, args: &str, ctx: &mut Context, site: &Site) -> Option<String> {
    match name {
        "include" => {
            let rest = args.trim_start();
            if rest.len() == args.len() {
                return None;
            }
            let file = &rest[..rest.find([' ', '>']).unwrap_or(rest.len())];
            (!file.is_empty()).then(|| include(ctx, file, site))
        }
        "pagebreak" => Some("<div class=\"pagebreak\"></div>".to_string()),
        "kbd" => kbd(args),
        "video" => video(args),
        "meta" => Some(escape(&lookup(ctx.meta, args.trim())?)),
        "var" => Some(escape(&variable(ctx, args.trim())?)),
        "lipsum" => lipsum(args),
        "placeholder" => Some(placeholder(args)),
        // No environment offline.
        "env" => Some(String::new()),
        // Not expanded: a line holding it is shown verbatim by the driver.
        _ => None,
    }
}

/// The positional arguments of a shortcode and its `key=value` ones.
fn arguments(args: &str) -> (Vec<String>, Vec<(String, String)>) {
    let (mut positional, mut named) = (Vec::new(), Vec::new());
    let mut rest = args.trim();
    while !rest.is_empty() {
        let token_end = |t: &str| t.find(char::is_whitespace).unwrap_or(t.len());
        let quoted = |t: &str| {
            let q = t.chars().next().filter(|c| matches!(c, '"' | '\''))?;
            let close = t[1..].find(q)?;
            Some((t[1..1 + close].to_string(), 2 + close))
        };
        let key_end = rest
            .find(|c: char| c == '=' || c.is_whitespace())
            .filter(|&k| rest[k..].starts_with('='));
        if let Some(k) = key_end {
            let after = &rest[k + 1..];
            let (value, used) = quoted(after).unwrap_or_else(|| {
                let n = token_end(after);
                (after[..n].to_string(), n)
            });
            named.push((rest[..k].to_string(), value));
            rest = after[used..].trim_start();
        } else {
            let (value, used) = quoted(rest).unwrap_or_else(|| {
                let n = token_end(rest);
                (rest[..n].to_string(), n)
            });
            positional.push(value);
            rest = rest[used..].trim_start();
        }
    }
    (positional, named)
}

/// `Ctrl-C` as `<kbd>Ctrl</kbd>+<kbd>C</kbd>`; of `mac=`, `win=` and `linux=`
/// the first given is used.
fn kbd(args: &str) -> Option<String> {
    let (positional, named) = arguments(args);
    let keys = positional.first().or_else(|| {
        named
            .iter()
            .find(|(k, _)| matches!(k.as_str(), "mac" | "win" | "linux"))
            .map(|(_, v)| v)
    })?;
    let parts: Vec<String> = keys
        .split('-')
        .filter(|k| !k.is_empty())
        .map(|k| format!("<kbd>{}</kbd>", escape(k)))
        .collect();
    (!parts.is_empty()).then(|| parts.join("+"))
}

/// The id after `marker` in `url`, up to the next character an id cannot have.
fn id_after<'a>(url: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &url[url.find(marker)? + marker.len()..];
    let n = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    (n > 0).then(|| &rest[..n])
}

/// A YouTube or Vimeo page address as the address of its embeddable player.
fn embed_url(url: &str) -> Option<String> {
    let host = url.split("://").nth(1)?;
    let host = host.split(['/', '?']).next()?;
    if host.ends_with("youtube.com") || host.ends_with("youtube-nocookie.com") {
        let id = id_after(url, "v=").or_else(|| id_after(url, "/embed/"))?;
        Some(format!("https://www.youtube.com/embed/{id}"))
    } else if host == "youtu.be" {
        let id = id_after(url, "youtu.be/")?;
        Some(format!("https://www.youtube.com/embed/{id}"))
    } else if host.ends_with("vimeo.com") {
        let id = id_after(url, "/video/").or_else(|| id_after(url, "vimeo.com/"))?;
        Some(format!("https://player.vimeo.com/video/{id}"))
    } else {
        None
    }
}

fn video(args: &str) -> Option<String> {
    let (positional, named) = arguments(args);
    let url = positional.first()?;
    let title = named
        .iter()
        .find(|(k, _)| k == "title")
        .map_or("Video", |(_, v)| v.as_str());
    let inner = match embed_url(url) {
        Some(embed) => format!(
            "<iframe src=\"{}\" title=\"{}\" allowfullscreen loading=\"lazy\"></iframe>",
            escape(&embed),
            escape(title)
        ),
        None => format!("<video controls src=\"{}\"></video>", escape(url)),
    };
    Some(format!("<div class=\"video\">{inner}</div>"))
}

/// The value at a dotted key of a front matter or `_variables.yml` map.
fn lookup(map: &[(String, Yaml)], key: &str) -> Option<String> {
    let key = key.split_whitespace().next()?;
    let mut parts = key.split('.');
    let mut current = get(map, parts.next()?)?;
    for part in parts {
        current = match current {
            Yaml::Map(m) => get(m, part)?,
            Yaml::List(items) => items.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current.text())
}

/// A `{{< var >}}` value from `_variables.yml` in the project root.
fn variable(ctx: &Context, key: &str) -> Option<String> {
    let text = (ctx.files)("_variables.yml")?;
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.first().map(|l| l.trim()) != Some("---") {
        lines.insert(0, "---");
        lines.push("---");
    }
    lookup(&parse_front_matter(&lines).meta, key)
}

const LIPSUM: [&str; 3] = [
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.",
    "Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.",
    "Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.",
];

/// `N` paragraphs of lorem ipsum, at most 50.
fn lipsum(args: &str) -> Option<String> {
    let (positional, _) = arguments(args);
    let n = match positional.first() {
        Some(n) => n.parse::<usize>().ok()?.min(50),
        None => 1,
    };
    Some(
        (0..n)
            .map(|k| LIPSUM[k % LIPSUM.len()])
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// A grey box that says its size.
fn placeholder(args: &str) -> String {
    let (positional, _) = arguments(args);
    let size = |k: usize, default: u32| {
        positional
            .get(k)
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(default)
            .min(4000)
    };
    let (w, h) = (size(0, 300), size(1, 150));
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"{w}x{h}\"><rect width=\"100%\" height=\"100%\" fill=\"#ccc\"/><text x=\"50%\" y=\"50%\" fill=\"#666\" font-family=\"sans-serif\" font-size=\"14\" text-anchor=\"middle\" dominant-baseline=\"middle\">{w} x {h}</text></svg>"
    )
}

/// The raw format of a fence's info (`{=html}` is `html`).
fn raw_format(info: &str) -> Option<&str> {
    let inner = info.strip_prefix("{=")?.strip_suffix('}')?.trim();
    (!inner.is_empty() && !inner.contains(char::is_whitespace)).then_some(inner)
}

/// A raw block fence opening at line `start`: `{=html}` becomes HTML in the
/// draft, any other format is dropped from it. Returns the line of the closing
/// fence (the last line if there is none), or none for an ordinary fence.
///
/// The HTML goes in a `<div class="raw-html">` of its own, and each blank line
/// of it becomes an empty comment, so the whole block stays one CommonMark
/// HTML block (which would otherwise end at the first blank line and let the
/// rest be parsed as Markdown). A dropped block leaves an empty comment
/// claiming its lines, so it has no rendered block of its own.
pub(crate) fn raw_block(
    lines: &[&str],
    start: usize,
    fence: (u8, usize),
    info: &str,
    out: &mut Out,
) -> Option<usize> {
    let format = raw_format(info)?;
    let end = (start + 1..lines.len())
        .find(|&k| fence_closes(lines[k], fence.0, fence.1))
        .unwrap_or(lines.len());
    let last = end.min(lines.len() - 1);
    let html = if format.eq_ignore_ascii_case("html") {
        let body: Vec<&str> = lines[start + 1..end]
            .iter()
            .map(|l| if l.trim().is_empty() { "<!-- -->" } else { *l })
            .collect();
        format!("<div class=\"raw-html\">\n{}\n</div>", body.join("\n"))
    } else {
        "<!-- -->".to_string()
    };
    out.block(&html, (start, last));
    Some(last)
}

/// `` `<b>x</b>`{=html} `` as its HTML, and other formats as nothing.
fn raw_inline(line: &str) -> String {
    if !line.contains("`{=") {
        return line.to_string();
    }
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let (mut copied, mut i) = (0, 0);
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let run = b[i..].iter().take_while(|&&c| c == b'`').count();
        let content = i + run;
        let ticks = &line[i..content];
        // The closing run is exactly as long as the opening one.
        let mut from = content;
        let close = loop {
            match line[from..].find(ticks) {
                None => break None,
                Some(k) => {
                    let at = from + k;
                    let more = b[at..].iter().take_while(|&&c| c == b'`').count();
                    if more == run {
                        break Some(at);
                    }
                    from = at + more;
                }
            }
        };
        let Some(close) = close else {
            i = content;
            continue;
        };
        let after = close + run;
        let format = line[after..].strip_prefix("{=").and_then(|r| {
            let n = r.find('}')?;
            Some((&r[..n], after + 2 + n + 1))
        });
        match format {
            Some((f, next)) if !f.is_empty() => {
                out.push_str(&line[copied..i]);
                if f.eq_ignore_ascii_case("html") {
                    out.push_str(&line[content..close]);
                }
                copied = next;
                i = next;
            }
            _ => i = after,
        }
    }
    out.push_str(&line[copied..]);
    out
}

/// A line with its shortcodes expanded.
pub(crate) fn expand_line(line: &str, ctx: &mut Context, site: &Site) -> String {
    let line = &raw_inline(line);
    let mut out = String::with_capacity(line.len());
    let (mut copied, mut scan) = (0, 0);
    while let Some(call) = find_shortcode(line, scan) {
        // `{{{< x >}}}` is the text `{{< x >}}`.
        if escaped(line, call.start) && line[call.end..].starts_with('}') {
            out.push_str(&line[copied..call.start - 1]);
            out.push_str(&line[call.start..call.end]);
            copied = call.end + 1;
            scan = copied;
            continue;
        }
        match expand(call.name, call.args, ctx, site) {
            Some(text) => {
                out.push_str(&line[copied..call.start]);
                out.push_str(&text);
                copied = call.end;
                scan = call.end;
            }
            None => scan = call.start + 3,
        }
    }
    out.push_str(&line[copied..]);
    out
}

/// Finds an include's file: relative to the including file, never absolute,
/// never climbing above the root.
fn resolve(ctx: &Context, name: &str, source_name: &str) -> Option<(String, String)> {
    let requested = name.replace('\\', "/");
    let drive = requested.as_bytes().get(1) == Some(&b':')
        && requested
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic());
    if requested.is_empty()
        || requested.starts_with('/')
        || drive
        || requested.split('/').any(str::is_empty)
    {
        return None;
    }
    let mut parts: Vec<&str> = source_name.split('/').collect();
    parts.pop();
    let mut normalized: Vec<&str> = Vec::new();
    for part in parts.into_iter().chain(requested.split('/')) {
        match part {
            "." => {}
            ".." => {
                normalized.pop()?;
            }
            _ => normalized.push(part),
        }
    }
    let normalized = normalized.join("/");
    (ctx.files)(&normalized)
        .map(|text| (normalized.clone(), text))
        .or_else(|| (ctx.files)(&requested).map(|text| (requested.clone(), text)))
}

fn include(ctx: &mut Context, name: &str, site: &Site) -> String {
    let Site {
        depth,
        seen,
        source_name,
        line,
    } = *site;
    let Some((normalized, text)) = resolve(ctx, name, source_name) else {
        ctx.report(
            "quarto_include_unauthorized",
            format!("name={name}"),
            source_name,
            line,
        );
        let message = ctx
            .strings
            .include_unavailable
            .replace("{name}", &escape(name));
        return format!("<span class=\"quarto-diagnostic\">{message}</span>");
    };
    if depth >= MAX_INCLUDE_DEPTH || seen.contains(&normalized) {
        ctx.report(
            "quarto_include_cycle",
            format!("normalized={normalized}"),
            source_name,
            line,
        );
        let message = ctx
            .strings
            .include_cycle
            .replace("{normalized}", &escape(&normalized));
        return format!("<span class=\"quarto-diagnostic\">{message}</span>");
    }
    let mut next = seen.to_vec();
    next.push(normalized.clone());
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    let parsed = {
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        parse(&borrowed)
    };
    let mut defaults = ctx.options.clone();
    if let Some(Yaml::Map(m)) = get(&parsed.fm.meta, "execute") {
        for (k, v) in m {
            set(&mut defaults, k, v.clone());
        }
    }
    // Cells an include hides in Quarto's output are hidden here too.
    for cell in &parsed.cells {
        let hidden = |key: &str| option(&cell.options, &defaults, key) == Some(&Yaml::Bool(false));
        if hidden("include") || hidden("echo") {
            for l in &mut lines[cell.start..=cell.end] {
                l.clear();
            }
        }
    }
    let mut fence: Option<(u8, usize)> = None;
    let mut out = Vec::with_capacity(lines.len());
    for line in &lines {
        if let Some((ch, len)) = fence {
            if fence_closes(line, ch, len) {
                fence = None;
            }
            out.push(line.clone());
            continue;
        }
        if let Some((ch, len, _)) = fence_open(line) {
            fence = Some((ch, len));
        }
        let inner = Site {
            depth: depth + 1,
            seen: &next,
            source_name: &normalized,
            line: 0,
        };
        out.push(expand_line(line, ctx, &inner));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::{compose, with_files};
    use crate::quarto::{draft, Extras};

    fn html(source: &str) -> String {
        let d = compose(source);
        comrak::markdown_to_html(&d.markdown, &crate::markdown::quarto_options())
    }

    #[test]
    fn an_unsupported_shortcode_is_kept_with_its_line() {
        assert_eq!(
            compose("Before {{< nosuch clip.mp4 >}} after\n").markdown,
            "```\nBefore {{< nosuch clip.mp4 >}} after\n```\n"
        );
    }

    #[test]
    fn pagebreak_kbd_and_env_expand_alone_and_inline() {
        assert_eq!(
            compose("{{< pagebreak >}}").markdown,
            "<div class=\"pagebreak\"></div>"
        );
        assert_eq!(
            compose("Press {{< kbd Ctrl-Shift-C >}} now").markdown,
            "Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> now"
        );
        assert_eq!(
            compose("{{< kbd mac=Command-C win=Ctrl-C >}}").markdown,
            "<kbd>Command</kbd>+<kbd>C</kbd>"
        );
        assert_eq!(compose("a{{< env HOME >}}b").markdown, "ab");
    }

    #[test]
    fn video_embeds_known_hosts_and_plays_other_urls() {
        let d = compose("{{< video https://www.youtube.com/watch?v=abc_123 title=\"Talk\" >}}");
        assert_eq!(
            d.markdown,
            "<div class=\"video\"><iframe src=\"https://www.youtube.com/embed/abc_123\" title=\"Talk\" allowfullscreen loading=\"lazy\"></iframe></div>"
        );
        assert!(compose("{{< video https://youtu.be/xyz >}}")
            .markdown
            .contains("youtube.com/embed/xyz"));
        assert!(compose("{{< video https://vimeo.com/12345 >}}")
            .markdown
            .contains("src=\"https://player.vimeo.com/video/12345\""));
        assert_eq!(
            compose("{{< video clip.mp4 >}}").markdown,
            "<div class=\"video\"><video controls src=\"clip.mp4\"></video></div>"
        );
    }

    #[test]
    fn meta_and_var_read_the_front_matter_and_variables_file() {
        let d = with_files(
            "---\ntitle: A & B\nauthor:\n  name: Ada\n---\n\n{{< meta title >}} by {{< meta author.name >}}; {{< var version >}} {{< var org.name >}} {{< var nope >}}",
            &[("_variables.yml", "version: 1.2\norg:\n  name: Acme\n")],
        );
        assert!(
            d.markdown
                .contains("A &amp; B by Ada; 1.2 Acme {{< var nope >}}"),
            "{}",
            d.markdown
        );
    }

    #[test]
    fn lipsum_and_placeholder_generate_text_and_a_grey_box() {
        let d = compose("{{< lipsum 2 >}}");
        assert_eq!(d.markdown.matches("\n\n").count(), 1, "{}", d.markdown);
        assert!(d.markdown.starts_with("Lorem ipsum dolor sit amet"));
        let d = compose("{{< placeholder 200 100 >}}");
        assert!(d.markdown.starts_with("<svg"), "{}", d.markdown);
        assert!(d.markdown.contains("width=\"200\" height=\"100\""));
    }

    #[test]
    fn an_escaped_shortcode_is_the_literal_text() {
        let d = compose("Write {{{< pagebreak >}}} to break.");
        assert_eq!(d.markdown, "Write {{< pagebreak >}} to break.");
    }

    #[test]
    fn a_raw_html_block_stays_one_html_block_and_other_formats_vanish() {
        let source = "Before\n\n```{=html}\n<p>one</p>\n\n<p>two *x*</p>\n```\n\n```{=latex}\n\\newpage\n```\n\nAfter\n";
        let out = html(source);
        assert!(
            out.contains("<p>one</p>\n<!-- -->\n<p>two *x*</p>"),
            "{out}"
        );
        assert!(!out.contains("newpage") && !out.contains("```"), "{out}");
        assert!(out.contains("<p>After</p>"), "{out}");
        // The lines of the dropped block are claimed by it.
        let d = compose(source);
        assert!(d.origin.contains(&(10, 12)), "{:?}", d.origin);
    }

    #[test]
    fn raw_inline_spans_pass_html_and_drop_other_formats() {
        let out = html("A `<b>x</b>`{=html} and `\\textbf{y}`{=latex} and `code`.\n");
        assert!(out.contains("A <b>x</b> and  and <code>code</code>."), "{out}");
    }

    #[test]
    fn includes_expand_and_nest_and_stay_opaque_in_code() {
        let d = with_files(
            "Before {{< include outer.qmd >}} after.",
            &[
                (
                    "outer.qmd",
                    "Outer {{< include inner.qmd >}}\n\n```{r}\n{{< include secret.qmd >}}\n```",
                ),
                ("inner.qmd", "Inner text"),
                ("secret.qmd", "SECRET"),
            ],
        );
        assert!(d.markdown.contains("Outer Inner text"), "{}", d.markdown);
        assert!(d.markdown.contains("{{< include secret.qmd >}}"));
        assert!(!d.markdown.contains("SECRET"));
        // Included text has no one-to-one source line.
        assert!(d.line_map.iter().all(Option::is_none));
    }

    #[test]
    fn include_cycles_and_unavailable_files_are_reported() {
        let d = with_files(
            "{{< include a.qmd >}}",
            &[
                ("a.qmd", "A {{< include b.qmd >}}"),
                ("b.qmd", "B {{< include a.qmd >}}"),
            ],
        );
        assert!(
            d.markdown.contains("Include cycle at a.qmd"),
            "{}",
            d.markdown
        );
        let cycle = d
            .diagnostics
            .iter()
            .find(|x| x.message == "quarto_include_cycle")
            .unwrap();
        assert_eq!(cycle.hints, vec!["normalized=a.qmd".to_string()]);
        assert_eq!(cycle.file, "b.qmd");

        let d = with_files(
            "{{< include ../secret.qmd >}}",
            &[("../secret.qmd", "must never appear"), ("safe.qmd", "safe")],
        );
        assert!(!d.markdown.contains("must never appear"));
        assert!(d
            .markdown
            .contains("Include ../secret.qmd is not available"));
        let unauthorized = &d.diagnostics[0];
        assert_eq!(unauthorized.message, "quarto_include_unauthorized");
        assert_eq!(unauthorized.hints, vec!["name=../secret.qmd".to_string()]);
        assert_eq!((unauthorized.file.as_str(), unauthorized.line), ("", 1));
    }

    #[test]
    fn an_included_hidden_cell_is_dropped() {
        let d = with_files(
            "{{< include hidden.qmd >}}",
            &[(
                "hidden.qmd",
                "```{r}\n#| include: false\nsecret_hidden_code()\n```\nVisible prose",
            )],
        );
        assert!(!d.markdown.contains("secret_hidden_code"));
        assert!(d.markdown.contains("Visible prose"));
    }

    #[test]
    fn includes_resolve_relative_to_the_including_file() {
        let read = |p: &str| (p == "chap/part.qmd").then(|| "Part".to_string());
        let d = draft(
            "{{< include chap/part.qmd >}}",
            "main.qmd",
            &read,
            &Extras::default(),
            false,
        );
        assert_eq!(d.markdown, "Part");
    }
}
