//! Turns a field's rustdoc comment into the contract's `description` and
//! `details` (SPEC §4.2, §14.7).
//!
//! - The first paragraph is the description, on one line, without a final
//!   period: "HTTP listen port." becomes "HTTP listen port".
//! - Every later paragraph, heading, list or code block is details, as
//!   CommonMark. Rustdoc is Markdown already, so only what is specific to
//!   rustdoc is converted: intra-doc links become code spans, and code
//!   blocks lose their doctest attributes and hidden (`# `) lines.
//!
//! A comment that does not start with a paragraph (it starts with a
//! heading, a list, a quote or a code block) is all description, as before
//! details existed.

use syn::{Attribute, Expr, ExprLit, Lit};

/// The `///` (or `#[doc = "..."]`) lines of a field, unindented as rustdoc
/// does: the common leading whitespace is removed, and the ` * ` margin of a
/// `/** */` block comment.
pub(crate) fn doc_lines(attrs: &[Attribute]) -> Vec<String> {
    let mut lines = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("doc")) {
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
            {
                let v = s.value();
                if v.contains('\n') {
                    lines.extend(block_comment_lines(&v));
                } else {
                    lines.push(v);
                }
            }
        }
    }
    unindent(lines)
}

/// The lines of a `/** */` comment, without the leading ` * ` margin when
/// every non-blank line has one.
fn block_comment_lines(s: &str) -> Vec<String> {
    let lines: Vec<&str> = s.lines().collect();
    let starred = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.trim_start().starts_with('*'));
    lines
        .into_iter()
        .map(|l| {
            if starred && !l.trim().is_empty() {
                let t = l.trim_start().strip_prefix('*').unwrap_or(l);
                t.strip_prefix(' ').unwrap_or(t).to_string()
            } else {
                l.to_string()
            }
        })
        .collect()
}

fn unindent(lines: Vec<String>) -> Vec<String> {
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out: Vec<String> = lines
        .into_iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                l[indent..].trim_end().to_string()
            }
        })
        .collect();
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

/// Splits doc comment lines into a description and details. Details are
/// empty when the comment has a single paragraph.
pub(crate) fn split_doc(lines: &[String]) -> (String, String) {
    if lines.is_empty() {
        return (String::new(), String::new());
    }
    let refs = link_refs(lines);
    if !starts_paragraph(&lines[0]) {
        return (one_line(lines, &refs), String::new());
    }
    let end = lines
        .iter()
        .position(|l| l.is_empty())
        .unwrap_or(lines.len());
    let desc = one_line(&lines[..end], &refs);
    let details = to_commonmark(&lines[end..], &refs);
    (desc, details)
}

/// Whether a line opens a CommonMark paragraph rather than a heading, a
/// list, a quote, a code block, a table or a thematic break.
fn starts_paragraph(line: &str) -> bool {
    let t = line.trim_start();
    if line.len() - t.len() >= 4 {
        return false; // indented code
    }
    if t.starts_with('#')
        || t.starts_with("```")
        || t.starts_with("~~~")
        || t.starts_with('>')
        || t.starts_with('|')
        || t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("+ ")
        || t == "---"
        || t == "***"
    {
        return false;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    !(digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") ")))
}

/// The first paragraph on one line, without a final period.
fn one_line(lines: &[String], refs: &[(String, String)]) -> String {
    let joined = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut s = convert_links(&joined, refs);
    if s.ends_with('.') && !s.ends_with("..") {
        s.pop();
    }
    s
}

/// Link reference definitions (`[label]: target`) whose target is a Rust
/// path rather than a URL: rustdoc resolves those as intra-doc links.
fn link_refs(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter_map(|l| ref_definition(l))
        .filter(|(_, target)| is_rust_path(target))
        .collect()
}

fn ref_definition(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    if line.len() - t.len() >= 4 {
        return None;
    }
    let rest = t.strip_prefix('[')?;
    let close = rest.find("]:")?;
    let label = &rest[..close];
    let target = rest[close + 2..].trim();
    if label.is_empty() || target.is_empty() || target.contains(' ') {
        return None;
    }
    Some((label.to_lowercase(), target.to_string()))
}

/// Whether a link target is an intra-doc link: a Rust path, optionally
/// with a disambiguator (`struct@Foo`, `Foo()`, `foo!`), rather than a URL
/// or an anchor.
fn is_rust_path(target: &str) -> bool {
    let t = target.trim_matches('`');
    let t = t.split_once('@').map_or(t, |(_, p)| p);
    let t = t
        .strip_suffix("()")
        .or_else(|| t.strip_suffix('!'))
        .unwrap_or(t);
    let t = t.split_once('#').map_or(t, |(p, _)| p);
    !t.is_empty()
        && !target.contains("://")
        && !target.starts_with('#')
        && !target.starts_with('/')
        && !target.starts_with("mailto:")
        && t.split("::").all(|seg| {
            let mut c = seg.chars();
            c.next().is_some_and(|ch| ch.is_alphabetic() || ch == '_')
                && c.all(|ch| ch.is_alphanumeric() || ch == '_')
        })
}

/// The text a link to a Rust item shows: its label as a code span, with any
/// disambiguator dropped.
fn code_span(label: &str) -> String {
    let inner = label.trim();
    if inner.starts_with('`') && inner.ends_with('`') && inner.len() >= 2 {
        return inner.to_string();
    }
    let inner = inner.split_once('@').map_or(inner, |(_, p)| p);
    format!("`{inner}`")
}

/// Rewrites intra-doc links in one line of text, outside code spans:
/// `` [`Foo`] ``, `[Foo]`, `[text](crate::Foo)` and `[text][label]` (with a
/// Rust-path definition) become code spans, or the link text when it is
/// not code. Other links are left alone.
fn convert_links(line: &str, refs: &[(String, String)]) -> String {
    let lookup = |label: &str| refs.iter().any(|(l, _)| *l == label.to_lowercase());
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            out.push(c);
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '`' {
            // Copy a code span unchanged.
            let run = chars[i..].iter().take_while(|&&x| x == '`').count();
            let fence: String = "`".repeat(run);
            let rest: String = chars[i + run..].iter().collect();
            if let Some(end) = rest.find(&fence) {
                let len = run + rest[..end].chars().count() + run;
                out.extend(&chars[i..i + len]);
                i += len;
            } else {
                out.push_str(&fence);
                i += run;
            }
            continue;
        }
        if c == '[' {
            if let Some((text, after)) = bracket(&chars, i) {
                // [text](target)
                if chars.get(after) == Some(&'(') {
                    if let Some(close) = chars[after..].iter().position(|&x| x == ')') {
                        let target: String = chars[after + 1..after + close].iter().collect();
                        if is_rust_path(target.trim()) {
                            out.push_str(&link_text(&text));
                            i = after + close + 1;
                            continue;
                        }
                    }
                }
                // [text][label]
                if chars.get(after) == Some(&'[') {
                    if let Some((label, end)) = bracket(&chars, after) {
                        let label = if label.is_empty() {
                            text.clone()
                        } else {
                            label
                        };
                        if lookup(&label) {
                            out.push_str(&link_text(&text));
                            i = end;
                            continue;
                        }
                    }
                }
                // [label], a shortcut reference
                if !matches!(chars.get(after), Some('(' | '[' | ':'))
                    && (lookup(&text) || is_item_link(&text))
                {
                    out.push_str(&code_span(&text));
                    i = after;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Whether `[text]` alone is an intra-doc link: a path in a code span, or
/// a bare path with `::`. A bare word such as `[x]` may be plain text.
fn is_item_link(text: &str) -> bool {
    is_rust_path(text) && (text.starts_with('`') || text.contains("::"))
}

/// The text of a link to a Rust item: a code span when the text is code or
/// a path, and the text itself otherwise.
fn link_text(text: &str) -> String {
    if text.starts_with('`') || is_item_link(text) {
        code_span(text)
    } else {
        text.to_string()
    }
}

/// The contents of the `[...]` starting at `start`, and the index after it.
fn bracket(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut depth = 0;
    let mut in_code = false;
    for (j, &ch) in chars.iter().enumerate().skip(start) {
        match ch {
            '`' => in_code = !in_code,
            '[' if !in_code => depth += 1,
            ']' if !in_code => {
                depth -= 1;
                if depth == 0 {
                    return Some((chars[start + 1..j].iter().collect(), j + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// Converts rustdoc Markdown to CommonMark: intra-doc links outside code
/// become code spans, their reference definitions go, and code fences lose
/// doctest attributes and hidden lines.
fn to_commonmark(lines: &[String], refs: &[(String, String)]) -> String {
    let mut out: Vec<String> = Vec::new();
    // The fence that opened the current code block, and whether it is Rust.
    let mut fence: Option<(String, bool)> = None;
    for line in lines {
        let t = line.trim_start();
        if let Some((open, rust)) = &fence {
            if t.starts_with(open.as_str())
                && t.trim_start_matches(open.chars().next().unwrap())
                    .trim()
                    .is_empty()
            {
                out.push(line.clone());
                fence = None;
                continue;
            }
            if *rust {
                // Rustdoc hides "# " lines from the rendered example.
                if t == "#" || t.starts_with("# ") {
                    continue;
                }
                if let Some(rest) = t.strip_prefix("##") {
                    let indent = &line[..line.len() - t.len()];
                    out.push(format!("{indent}#{rest}"));
                    continue;
                }
            }
            out.push(line.clone());
            continue;
        }
        if let Some((open, info)) = fence_open(line) {
            let (rust, lang) = rustdoc_lang(&info);
            let indent = &line[..line.len() - t.len()];
            out.push(format!("{indent}{open}{lang}"));
            fence = Some((open, rust));
            continue;
        }
        if line.len() - t.len() >= 4 {
            out.push(line.clone()); // indented code
            continue;
        }
        if let Some((label, _)) = ref_definition(line) {
            if refs.iter().any(|(l, _)| *l == label) {
                continue;
            }
        }
        out.push(convert_links(line, refs));
    }
    // Dropping reference definitions can leave blank runs.
    let mut s = String::new();
    let mut blank = true;
    for l in out {
        if l.is_empty() {
            if blank {
                continue;
            }
            blank = true;
        } else {
            blank = false;
        }
        s.push_str(&l);
        s.push('\n');
    }
    s.trim().to_string()
}

/// The fence and info string of a line that opens a fenced code block.
fn fence_open(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    if line.len() - t.len() >= 4 {
        return None;
    }
    let ch = t.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let n = t.chars().take_while(|&c| c == ch).count();
    if n < 3 {
        return None;
    }
    let info = t[n..].trim().to_string();
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch.to_string().repeat(n), info))
}

/// The CommonMark info string for a rustdoc code block, and whether the
/// block is Rust. Rustdoc reads an empty info string, or one made only of
/// doctest attributes, as Rust.
fn rustdoc_lang(info: &str) -> (bool, String) {
    const DOCTEST: &[&str] = &[
        "rust",
        "ignore",
        "no_run",
        "should_panic",
        "compile_fail",
        "test_harness",
        "standalone_crate",
    ];
    let words: Vec<&str> = info
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect();
    let is_attr = |w: &&str| {
        DOCTEST.contains(w)
            || w.starts_with("edition")
            || w.starts_with("ignore-")
            || w.starts_with("E0")
    };
    if words.iter().all(is_attr) {
        return (true, "rust".into());
    }
    let lang: Vec<&str> = words.into_iter().filter(|w| !is_attr(w)).collect();
    (false, lang.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(doc: &str) -> (String, String) {
        let lines: Vec<String> = doc.lines().map(|l| format!(" {l}")).collect();
        split_doc(&unindent(lines))
    }

    #[test]
    fn one_line() {
        assert_eq!(
            split("HTTP listen port."),
            ("HTTP listen port".into(), String::new())
        );
    }

    #[test]
    fn one_paragraph_over_several_lines() {
        assert_eq!(
            split("Certificate to serve HTTPS with.\nWithout it, the service serves HTTP."),
            (
                "Certificate to serve HTTPS with. Without it, the service serves HTTP".into(),
                String::new()
            )
        );
    }

    #[test]
    fn paragraphs() {
        assert_eq!(
            split("Number of workers.\n\nEach holds a database connection,\nso keep it below the pool size.\n\nRaise it when the queue grows."),
            (
                "Number of workers".into(),
                "Each holds a database connection,\nso keep it below the pool size.\n\nRaise it when the queue grows.".into()
            )
        );
    }

    #[test]
    fn heading_list_and_code() {
        let (d, details) = split(
            "Request timeout.\n\n# Choosing a value\n\nMeasure first:\n- p99 latency\n- retries\n\n```\n# use std::time::Duration;\nlet t = Duration::from_secs(30);\n## not hidden\n```\n\n```sh,ignore\ncurl $URL\n```\n\n    indented code\n    # stays",
        );
        assert_eq!(d, "Request timeout");
        assert_eq!(
            details,
            "# Choosing a value\n\nMeasure first:\n- p99 latency\n- retries\n\n```rust\nlet t = Duration::from_secs(30);\n# not hidden\n```\n\n```sh\ncurl $URL\n```\n\n    indented code\n    # stays"
        );
    }

    #[test]
    fn intra_doc_links_become_code_spans() {
        let (d, details) = split(
            "Upstream timeout, see [`Duration`].\n\nParsed by [`humantime::parse_duration`], unlike [std::time::Duration].\nSee [the loader](crate::Loader), [`Meta`](crate::Meta) and [Values][values].\nThe [docs](https://docs.rs/docuconf) and [`code` in links] stay.\n\n[values]: crate::Values",
        );
        assert_eq!(d, "Upstream timeout, see `Duration`");
        assert_eq!(
            details,
            "Parsed by `humantime::parse_duration`, unlike `std::time::Duration`.\nSee the loader, `Meta` and Values.\nThe [docs](https://docs.rs/docuconf) and [`code` in links] stay."
        );
    }

    #[test]
    fn code_spans_and_urls_are_untouched() {
        let (_, details) =
            split("Port.\n\nWrite `[Foo]` or [a](#anchor) or [b][c].\n\n[c]: https://example.com");
        assert_eq!(
            details,
            "Write `[Foo]` or [a](#anchor) or [b][c].\n\n[c]: https://example.com"
        );
    }

    #[test]
    fn non_ascii() {
        assert_eq!(
            split("Grußtext für die Startseite.\n\nZeigt «ça va» und 東京."),
            (
                "Grußtext für die Startseite".into(),
                "Zeigt «ça va» und 東京.".into()
            )
        );
    }

    #[test]
    fn starts_with_a_list_is_all_description() {
        assert_eq!(
            split("- first\n- second"),
            ("- first - second".into(), String::new())
        );
    }

    #[test]
    fn block_comment() {
        let lines = block_comment_lines("*\n * Queue depth.\n *\n * Alert above 10.\n ");
        assert_eq!(
            split_doc(&unindent(lines)),
            ("Queue depth".into(), "Alert above 10.".into())
        );
    }
}
