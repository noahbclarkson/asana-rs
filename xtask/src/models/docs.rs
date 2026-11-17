//! Turns the specification's descriptions into short, rustdoc-safe doc comments.
//!
//! Asana's descriptions are Markdown with HTML, relative links and several paragraphs. A doc
//! comment keeps the first sentence or two on one line, drops tags and link targets, escapes
//! brackets that rustdoc would read as intra-doc links, and wraps bare URLs.

/// The longest summary kept, in characters, before it is cut at a sentence end.
const MAX_LENGTH: usize = 320;

/// A one-line summary of `text`, or `None` when nothing usable is left.
pub fn summary(text: &str) -> Option<String> {
    let text = strip_tags(text);
    let text = unlink(&text);
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // A Markdown heading starts the description's reference part; the summary stops before it.
    let text = text
        .find(" #")
        .map_or(text.as_str(), |heading| &text[..heading]);
    let text = first_sentences(text);
    let text = escape(text.trim());
    (!text.is_empty()).then_some(text)
}

/// Removes `<tag ...>` and `</tag>` but keeps the text between them.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let looks_like_tag = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!');
        match (looks_like_tag, after.find('>')) {
            (true, Some(end)) => rest = &after[end + 1..],
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Replaces `[text](target)` with `text`.
fn unlink(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let link = after.find("](").and_then(|middle| {
            let target = &after[middle + 2..];
            target
                .find(')')
                .map(|close| (middle, middle + 2 + close + 1))
        });
        match link {
            Some((middle, end)) if !after[..middle].contains('[') => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..middle]);
                rest = &after[end..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The first one or two sentences, never longer than [`MAX_LENGTH`] unless the first sentence is.
fn first_sentences(text: &str) -> &str {
    let mut ends = Vec::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut offset = 0;
    for (index, c) in bytes.iter().enumerate() {
        if *c == '.'
            && bytes.get(index + 1) == Some(&' ')
            && bytes
                .get(index + 2)
                .is_some_and(|next| next.is_uppercase() || *next == '*' || *next == '`')
        {
            ends.push(offset + 1);
        }
        offset += c.len_utf8();
    }
    ends.push(text.len());
    match ends.as_slice() {
        [] | [_] => text,
        [first, ..] if *first > MAX_LENGTH => &text[..*first],
        [_, second, ..] if *second <= MAX_LENGTH => &text[..*second],
        [first, ..] => &text[..*first],
    }
}

/// Escapes what rustdoc would misread: brackets as intra-doc links, angle brackets as HTML, an
/// unbalanced backtick, and bare URLs.
fn escape(text: &str) -> String {
    let text = if text.matches('`').count() % 2 == 1 {
        text.replace('`', "")
    } else {
        text.to_owned()
    };
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    let mut rest = text.as_str();
    while let Some(c) = rest.chars().next() {
        if c == '`' {
            in_code = !in_code;
        }
        if !in_code && (rest.starts_with("https://") || rest.starts_with("http://")) {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == ')' || c == '"')
                .unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';']);
            out.push('<');
            out.push_str(url);
            out.push('>');
            rest = &rest[url.len()..];
            continue;
        }
        match c {
            '[' | ']' if !in_code => {
                out.push('\\');
                out.push(c);
            }
            '<' if !in_code => out.push_str("&lt;"),
            '>' if !in_code => out.push_str("&gt;"),
            _ => out.push(c),
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_is_the_first_sentences_on_one_line_without_markup() {
        assert_eq!(
            summary("[Opt In](/docs/inputoutput-options). A *user* object\nrepresents an account. More text follows here.").unwrap(),
            "Opt In. A *user* object represents an account."
        );
        assert_eq!(
            summary("<body>The <strong>notes</strong> [of] it</body>").unwrap(),
            "The notes \\[of\\] it"
        );
        assert_eq!(
            summary("See https://example.com/x. Then a < b.").unwrap(),
            "See <https://example.com/x>. Then a &lt; b."
        );
        assert_eq!(summary("  "), None);
    }
}
