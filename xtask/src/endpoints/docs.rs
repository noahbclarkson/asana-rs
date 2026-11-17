//! Turns the specification's prose into doc comments rustdoc renders cleanly.
//!
//! Asana's descriptions are HTML-flavoured Markdown: a `<b>Required scope: </b>` line, tables of
//! the scopes each field needs, `<code>` tags, Markdown links and bare URLs. A descriptor's doc
//! keeps the summary and the description's first sentence; everything rustdoc could misread
//! (tags, square brackets that are not links, bare URLs) is rewritten or dropped.

use proc_macro2::TokenStream;
use quote::quote;

/// The doc comment of a descriptor: the summary, the description's first sentence, and the
/// method and route in backticks.
pub fn operation(
    summary: Option<&str>,
    description: Option<&str>,
    deprecated: bool,
    method: &str,
    route: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(summary) = summary.map(clean).filter(|summary| !summary.is_empty()) {
        lines.push(full_stop(&summary));
    }
    if let Some(sentence) = description.and_then(first_sentence)
        && lines.first() != Some(&sentence)
    {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(sentence);
    }
    if deprecated {
        lines.push(String::new());
        lines.push("**Deprecated** in Asana's specification.".to_owned());
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(format!("`{method} {route}`"));
    lines
}

/// The first sentence of a description, cleaned, or `None` when it has no prose.
pub fn first_sentence(description: &str) -> Option<String> {
    first_paragraph(description).map(|paragraph| full_stop(&cut_sentence(&paragraph)))
}

/// `#[doc = "..."]` attributes for `lines`, each with the leading space `///` would have.
pub fn attributes(lines: &[String]) -> TokenStream {
    let lines = lines.iter().map(|line| {
        if line.is_empty() {
            String::new()
        } else {
            format!(" {line}")
        }
    });
    quote! { #( #[doc = #lines] )* }
}

/// A parameter's doc: the first paragraph of its description, or a fallback when the
/// specification says nothing.
pub fn or(description: Option<&str>, fallback: &str) -> Vec<String> {
    vec![
        description
            .and_then(first_paragraph)
            .unwrap_or_else(|| fallback.to_owned()),
    ]
}

/// The first paragraph of a description, cleaned and on one line.
fn first_paragraph(description: &str) -> Option<String> {
    let cleaned = clean(description);
    let paragraph = cleaned
        .split("\n\n")
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|paragraph| !paragraph.is_empty())?;
    Some(full_stop(&paragraph))
}

fn full_stop(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(['.', '!', '?', ':']) || text.ends_with(".*") || text.ends_with(".)") {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

/// Cuts at the first full stop that ends a sentence (followed by a space, not inside backticks and
/// not an abbreviation such as `e.g.`).
fn cut_sentence(paragraph: &str) -> String {
    let bytes = paragraph.as_bytes();
    let mut in_code = false;
    for (index, &byte) in bytes.iter().enumerate() {
        match byte {
            b'`' => in_code = !in_code,
            b'.' if !in_code && bytes.get(index + 1) == Some(&b' ') => {
                let before = &paragraph[..index];
                let word = before.rsplit(' ').next().unwrap_or_default();
                if !matches!(word, "e.g" | "i.e" | "etc" | "vs") {
                    return paragraph[..=index].to_owned();
                }
            }
            _ => {}
        }
    }
    paragraph.to_owned()
}

/// Removes what rustdoc would misread, keeping the prose.
pub fn clean(text: &str) -> String {
    let without_tables = remove_between(text, "<table", "</table>");
    let paragraphs: Vec<&str> = without_tables
        .split("\n\n")
        .filter(|paragraph| !paragraph.contains("Required scope"))
        .collect();
    let text = paragraphs.join("\n\n");
    let text = strip_tags(&text);
    let text = escape_brackets_and_urls(&text);
    balance_backticks(&text)
}

fn remove_between(text: &str, start: &str, end: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find(start) {
        out.push_str(&rest[..open]);
        match rest[open..].find(end) {
            Some(close) => rest = &rest[open + close + end.len()..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Replaces `<code>` and `</code>` with backticks, `<br>` with a line break, drops every other tag,
/// and writes a `<` that starts no tag as `&lt;`.
fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let looks_like_tag = after
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '/');
        match after.find('>') {
            Some(close) if looks_like_tag && !after[..close].contains('<') => {
                let tag = after[..close].trim_start_matches('/');
                let name: String = tag
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect::<String>()
                    .to_ascii_lowercase();
                match name.as_str() {
                    "code" => out.push('`'),
                    "br" | "p" | "li" => out.push('\n'),
                    _ => {}
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push_str("&lt;");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("&nbsp;", " ")
}

/// Keeps Markdown links whose target is a URL, escapes every other square bracket (rustdoc would
/// read it as an intra-doc link) and wraps bare URLs in angle brackets.
fn escape_brackets_and_urls(text: &str) -> String {
    let mut out = String::new();
    let mut index = 0;
    let bytes = text.as_bytes();
    let mut in_code = false;
    while index < text.len() {
        let rest = &text[index..];
        let byte = bytes[index];
        if byte == b'`' {
            in_code = !in_code;
        }
        if in_code {
            let next = rest.chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[..next]);
            index += next;
            continue;
        }
        if byte == b'['
            && let Some((link, length)) = markdown_link(rest)
        {
            out.push_str(&link);
            index += length;
        } else if byte == b'[' || byte == b']' {
            out.push('\\');
            out.push(char::from(byte));
            index += 1;
        } else if rest.starts_with("https://") || rest.starts_with("http://") {
            let end = rest
                .find(|character: char| character.is_whitespace() || character == ')')
                .unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';']);
            out.push('<');
            out.push_str(url);
            out.push('>');
            index += url.len();
        } else {
            let next = rest.chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[..next]);
            index += next;
        }
    }
    out
}

/// A `[text](https://...)` link at the start of `text`, and its length. A link to a path on
/// Asana's documentation site (`/docs/...`) is made absolute.
fn markdown_link(text: &str) -> Option<(String, usize)> {
    let close = text.find("](")?;
    if text[1..close].contains(['[', ']', '\n']) {
        return None;
    }
    let target = &text[close + 2..];
    let end = target.find(')')?;
    let url = &target[..end];
    if url.contains(char::is_whitespace) {
        return None;
    }
    let absolute = if url.starts_with("https://") || url.starts_with("http://") {
        url.to_owned()
    } else if url.starts_with('/') {
        format!("https://developers.asana.com{url}")
    } else {
        return None;
    };
    Some((
        format!("{}]({absolute})", &text[..close]),
        close + 2 + end + 1,
    ))
}

/// Drops every backtick from a paragraph that has an odd number of them, so a cut sentence can
/// never leave a code span open.
fn balance_backticks(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| {
            if paragraph.matches('`').count() % 2 == 1 {
                paragraph.replace('`', "")
            } else {
                paragraph.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scope_line_and_tables_are_dropped_and_the_first_sentence_kept() {
        let description = "<b>Required scope: </b><code>tasks:read</code>\n\n<table>\n<tr><td><code>memberships</code></td></tr>\n</table>\n\nReturns the complete task record for a single task. More follows.";
        assert_eq!(
            first_sentence(description).as_deref(),
            Some("Returns the complete task record for a single task.")
        );
    }

    #[test]
    fn tags_become_markdown_and_brackets_and_urls_are_made_safe() {
        assert_eq!(
            clean(
                "Use <code>opt_fields</code> [here](https://example.com/a) or [x] at https://example.com/b."
            ),
            "Use `opt_fields` [here](https://example.com/a) or \\[x\\] at <https://example.com/b>."
        );
        assert_eq!(clean("a < b"), "a &lt; b");
        assert_eq!(
            clean("See the [guide](/docs/pagination)."),
            "See the [guide](https://developers.asana.com/docs/pagination)."
        );
    }

    #[test]
    fn a_sentence_is_not_cut_at_an_abbreviation_or_inside_code() {
        assert_eq!(
            cut_sentence("Use e.g. `a. b` here. Then more."),
            "Use e.g. `a. b` here."
        );
    }

    #[test]
    fn the_operation_doc_ends_with_the_method_and_route() {
        let lines = operation(Some("Get a task"), None, false, "GET", "/tasks/{task_gid}");
        assert_eq!(lines, ["Get a task.", "", "`GET /tasks/{task_gid}`"]);
    }
}
