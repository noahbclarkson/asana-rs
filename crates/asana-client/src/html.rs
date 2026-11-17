//! Asana's rich text, built so that a caller's text can never become markup.
//!
//! Asana takes rich text in `html_text` (comments, status updates) and `html_notes` (tasks,
//! projects). It must be one well-formed XML fragment inside a `<body>` element, using only the
//! tags Asana lists in its rich-text guide; anything else is refused with a `400`. Asana has no
//! paragraph or line-break tag: a newline is a line break and a blank line separates paragraphs.
//!
//! [`Html`] writes that format. Every piece of text it is given is escaped, every link's address
//! is checked and escaped, and a mention is a GID, which holds only digits. The one way to put
//! markup in unescaped is [`Html::raw_trusted`].

use std::fmt;

use asana_openapi::ids::Gid;

/// A rich-text document for `html_text` or `html_notes`.
///
/// Build it by chaining: each call appends to the current paragraph, and
/// [`next_paragraph`](Html::next_paragraph) starts another.
///
/// ```
/// use asana_client::Html;
/// use asana_client::openapi::ids::{Gid, UserGid};
///
/// let reviewer = UserGid::parse("1201234567890123").unwrap();
/// let comment = Html::paragraph("Approved by ")
///     .mention(&reviewer)
///     .text(". ")
///     .bold("Ready to send.")
///     .next_paragraph("Checked:")
///     .list(["Totals <= budget", "Names & dates"]);
/// assert_eq!(
///     comment.to_string(),
///     "<body>Approved by <a data-asana-gid=\"1201234567890123\"/>. <strong>Ready to send.</strong>\n\n\
///      Checked:<ul><li>Totals &lt;= budget</li><li>Names &amp; dates</li></ul></body>",
/// );
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Html {
    /// The markup inside `<body>`.
    inner: String,
}

impl Html {
    /// An empty document.
    pub fn new() -> Self {
        Self::default()
    }

    /// A document that starts with a paragraph of plain `text`.
    pub fn paragraph(text: impl AsRef<str>) -> Self {
        Self::new().text(text)
    }

    /// Markup the caller has already made valid for Asana, used exactly as given.
    ///
    /// It may be the inside of a `<body>` or a whole `<body>…</body>`; the wrapper is not
    /// doubled. Nothing is escaped or checked, so never pass text a user wrote: Asana refuses
    /// malformed markup and unknown tags with a `400`.
    pub fn raw_trusted(markup: impl Into<String>) -> Self {
        let markup = markup.into();
        let trimmed = markup.trim();
        let inner = trimmed
            .strip_prefix("<body>")
            .and_then(|rest| rest.strip_suffix("</body>"))
            .map_or_else(|| markup.clone(), str::to_owned);
        Self { inner }
    }

    /// Appends plain text. A newline in it is a line break.
    #[must_use]
    pub fn text(mut self, text: impl AsRef<str>) -> Self {
        escape_into(&mut self.inner, text.as_ref());
        self
    }

    /// Appends text in bold (`<strong>`).
    #[must_use]
    pub fn bold(self, text: impl AsRef<str>) -> Self {
        self.wrapped("strong", text.as_ref())
    }

    /// Appends text in italics (`<em>`).
    #[must_use]
    pub fn italic(self, text: impl AsRef<str>) -> Self {
        self.wrapped("em", text.as_ref())
    }

    /// Appends underlined text (`<u>`).
    #[must_use]
    pub fn underline(self, text: impl AsRef<str>) -> Self {
        self.wrapped("u", text.as_ref())
    }

    /// Appends struck-through text (`<s>`).
    #[must_use]
    pub fn strike(self, text: impl AsRef<str>) -> Self {
        self.wrapped("s", text.as_ref())
    }

    /// Appends inline code (`<code>`).
    #[must_use]
    pub fn code(self, text: impl AsRef<str>) -> Self {
        self.wrapped("code", text.as_ref())
    }

    /// Appends a link to `url` reading `text`.
    ///
    /// Only `http`, `https` and `mailto` addresses become links; any other address (a
    /// `javascript:` one, say) leaves `text` as plain text, so a user-supplied address cannot run
    /// anything where the comment is shown.
    #[must_use]
    pub fn link(mut self, url: impl AsRef<str>, text: impl AsRef<str>) -> Self {
        let url = url.as_ref().trim();
        if !is_safe_link(url) {
            return self.text(text);
        }
        self.inner.push_str("<a href=\"");
        escape_into(&mut self.inner, url);
        self.inner.push_str("\">");
        escape_into(&mut self.inner, text.as_ref());
        self.inner.push_str("</a>");
        self
    }

    /// Appends a mention of a user, task, project or any other Asana object, in Asana's
    /// `<a data-asana-gid="…"/>` form. Asana fills in the name; mentioning a user notifies them
    /// but does not make them a follower.
    #[must_use]
    pub fn mention<G: Gid>(mut self, gid: &G) -> Self {
        self.inner.push_str("<a data-asana-gid=\"");
        // A GID holds only digits, but escaping costs nothing and keeps the rule simple.
        escape_into(&mut self.inner, gid.as_str());
        self.inner.push_str("\"/>");
        self
    }

    /// Appends a line break.
    #[must_use]
    pub fn line_break(mut self) -> Self {
        self.inner.push('\n');
        self
    }

    /// Starts a new paragraph of plain `text`.
    #[must_use]
    pub fn next_paragraph(mut self, text: impl AsRef<str>) -> Self {
        if !self.inner.is_empty() {
            self.inner.push_str("\n\n");
        }
        self.text(text)
    }

    /// Appends a bulleted list (`<ul>`). Each item is plain text (escaped) or an [`Html`].
    #[must_use]
    pub fn list<I: Into<Self>>(self, items: impl IntoIterator<Item = I>) -> Self {
        self.items("ul", items)
    }

    /// Appends a numbered list (`<ol>`). Each item is plain text (escaped) or an [`Html`].
    #[must_use]
    pub fn numbered_list<I: Into<Self>>(self, items: impl IntoIterator<Item = I>) -> Self {
        self.items("ol", items)
    }

    /// Appends another document's content to this one.
    #[must_use]
    pub fn append(mut self, other: Self) -> Self {
        self.inner.push_str(&other.inner);
        self
    }

    /// Whether nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn wrapped(mut self, tag: &str, text: &str) -> Self {
        self.inner.push('<');
        self.inner.push_str(tag);
        self.inner.push('>');
        escape_into(&mut self.inner, text);
        self.inner.push_str("</");
        self.inner.push_str(tag);
        self.inner.push('>');
        self
    }

    fn items<I: Into<Self>>(mut self, tag: &str, items: impl IntoIterator<Item = I>) -> Self {
        self.inner.push('<');
        self.inner.push_str(tag);
        self.inner.push('>');
        for item in items {
            self.inner.push_str("<li>");
            self.inner.push_str(&item.into().inner);
            self.inner.push_str("</li>");
        }
        self.inner.push_str("</");
        self.inner.push_str(tag);
        self.inner.push('>');
        self
    }
}

/// Plain text, escaped.
impl From<&str> for Html {
    fn from(text: &str) -> Self {
        Self::new().text(text)
    }
}

/// Plain text, escaped.
impl From<String> for Html {
    fn from(text: String) -> Self {
        Self::new().text(text)
    }
}

/// The finished document, `<body>…</body>`, as Asana takes it.
impl fmt::Display for Html {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<body>")?;
        f.write_str(&self.inner)?;
        f.write_str("</body>")
    }
}

/// Writes `text` as XML character data that is also safe inside a double-quoted attribute.
/// Characters XML 1.0 forbids (most C0 controls) are dropped, because Asana refuses a document
/// that holds them.
fn escape_into(out: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\t' | '\n' => out.push(character),
            '\r' => {}
            control if control.is_control() && u32::from(control) < 0x20 => {}
            '\u{FFFE}' | '\u{FFFF}' => {}
            other => out.push(other),
        }
    }
}

fn is_safe_link(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && ["http", "https", "mailto"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
}

#[cfg(test)]
mod tests {
    use asana_openapi::ids::{TaskGid, UserGid};

    use super::*;

    fn gid<G: Gid>(text: &str) -> G {
        G::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn caller_text_cannot_open_a_tag_or_an_attribute() {
        let html = Html::paragraph("<script>alert('x')</script> & \"quotes\"");
        assert_eq!(
            html.to_string(),
            "<body>&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; &quot;quotes&quot;</body>"
        );
        let html = Html::new()
            .bold("</strong><img src=x>")
            .link("https://example.com/?a=1&b=\"2\"", "<b>");
        assert_eq!(
            html.to_string(),
            "<body><strong>&lt;/strong&gt;&lt;img src=x&gt;</strong>\
             <a href=\"https://example.com/?a=1&amp;b=&quot;2&quot;\">&lt;b&gt;</a></body>"
        );
    }

    #[test]
    fn controls_xml_forbids_are_dropped_and_newlines_kept() {
        let html = Html::paragraph("a\u{0}b\u{1b}c\r\nd\te");
        assert_eq!(html.to_string(), "<body>abc\nd\te</body>");
    }

    #[test]
    fn each_inline_element_has_its_tag() {
        let html = Html::new()
            .bold("b")
            .italic("i")
            .underline("u")
            .strike("s")
            .code("c");
        assert_eq!(
            html.to_string(),
            "<body><strong>b</strong><em>i</em><u>u</u><s>s</s><code>c</code></body>"
        );
    }

    #[test]
    fn only_web_and_mail_addresses_become_links() {
        let html = Html::new()
            .link("mailto:ops@example.com", "mail")
            .link("javascript:alert(1)", "js")
            .link("HTTPS://example.com", "web")
            .link("example.com", "bare");
        assert_eq!(
            html.to_string(),
            "<body><a href=\"mailto:ops@example.com\">mail</a>js\
             <a href=\"HTTPS://example.com\">web</a>bare</body>"
        );
    }

    #[test]
    fn a_mention_is_asanas_self_closing_link() {
        let user: UserGid = gid("1201234567890123");
        let task: TaskGid = gid("1209876543210987");
        let html = Html::paragraph("cc ")
            .mention(&user)
            .text(" on ")
            .mention(&task);
        assert_eq!(
            html.to_string(),
            "<body>cc <a data-asana-gid=\"1201234567890123\"/> on \
             <a data-asana-gid=\"1209876543210987\"/></body>"
        );
    }

    #[test]
    fn paragraphs_breaks_and_lists() {
        let html = Html::paragraph("one")
            .line_break()
            .text("two")
            .next_paragraph("three")
            .list(["a<", "b"])
            .numbered_list([Html::new().bold("x"), Html::from("y")]);
        assert_eq!(
            html.to_string(),
            "<body>one\ntwo\n\nthree<ul><li>a&lt;</li><li>b</li></ul>\
             <ol><li><strong>x</strong></li><li>y</li></ol></body>"
        );
        assert_eq!(
            Html::new().next_paragraph("first").to_string(),
            "<body>first</body>"
        );
    }

    #[test]
    fn trusted_markup_is_kept_and_not_double_wrapped() {
        assert_eq!(
            Html::raw_trusted("<body><em>x</em></body>").to_string(),
            "<body><em>x</em></body>"
        );
        assert_eq!(
            Html::raw_trusted("<em>x</em>")
                .append(Html::from("&"))
                .to_string(),
            "<body><em>x</em>&amp;</body>"
        );
    }
}
