//! Shortcodes: `{{< name args >}}`.
//!
//! Owner: g7cv. [`expand`] is the extension point: one match arm per
//! shortcode name, `include` today. A name with no arm (and no entry in
//! [`handled`]) is the default: the whole line stays as verbatim source.

use crate::page::escape;

use super::cells::option;
use super::front_matter::{get, set, Yaml};
use super::{fence_closes, fence_open, parse, Context};

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
}

/// Whether a line holds a shortcode other than the handled ones.
pub(crate) fn has_unsupported_shortcode(line: &str) -> bool {
    let mut pos = 0;
    while let Some(k) = line[pos..].find("{{<") {
        pos += k + 3;
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
        // Not expanded: a line holding it is shown verbatim by the driver.
        _ => None,
    }
}

/// A line with its shortcodes expanded.
pub(crate) fn expand_line(line: &str, ctx: &mut Context, site: &Site) -> String {
    let mut out = String::with_capacity(line.len());
    let (mut copied, mut scan) = (0, 0);
    while let Some(call) = find_shortcode(line, scan) {
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

    #[test]
    fn an_unsupported_shortcode_is_kept_with_its_line() {
        assert_eq!(
            compose("Before {{< video clip.mp4 >}} after\n").markdown,
            "```\nBefore {{< video clip.mp4 >}} after\n```\n"
        );
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
