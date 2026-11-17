//! Reads a `multipart/form-data` body (RFC 7578), for attachment uploads.

use bytes::Bytes;

/// One field of a form.
#[derive(Clone, Debug)]
pub(crate) struct FormPart {
    pub(crate) name: String,
    pub(crate) file_name: Option<String>,
    pub(crate) content_type: Option<String>,
    pub(crate) data: Bytes,
}

/// The boundary named by a `Content-Type` header, when it is a multipart form.
pub(crate) fn boundary(content_type: &str) -> Option<String> {
    let (kind, parameters) = content_type.split_once(';')?;
    if !kind.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    parameters.split(';').find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("boundary")
            .then(|| value.trim().trim_matches('"').to_owned())
    })
}

/// Splits `body` into its parts; `None` when it is not a well-formed form.
pub(crate) fn parse(body: &Bytes, boundary: &str) -> Option<Vec<FormPart>> {
    let delimiter = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let mut rest = body.as_ref();
    let start = find(rest, &delimiter)?;
    rest = rest.get(start + delimiter.len()..)?;
    loop {
        if rest.starts_with(b"--") {
            return Some(parts);
        }
        rest = rest.strip_prefix(b"\r\n")?;
        let end = find(rest, &delimiter)?;
        let part = rest.get(..end)?.strip_suffix(b"\r\n")?;
        let split = find(part, b"\r\n\r\n")?;
        let headers = std::str::from_utf8(part.get(..split)?).ok()?;
        let data = part.get(split + 4..)?;
        let mut name = None;
        let mut file_name = None;
        let mut content_type = None;
        for header in headers.split("\r\n") {
            let (key, value) = header.split_once(':')?;
            if key.trim().eq_ignore_ascii_case("content-disposition") {
                name = parameter(value, "name");
                file_name = parameter(value, "filename");
            } else if key.trim().eq_ignore_ascii_case("content-type") {
                content_type = Some(value.trim().to_owned());
            }
        }
        parts.push(FormPart {
            name: name?,
            file_name,
            content_type,
            data: body.slice_ref(data),
        });
        rest = rest.get(end + delimiter.len()..)?;
    }
}

/// A quoted parameter of a `Content-Disposition` value.
fn parameter(value: &str, key: &str) -> Option<String> {
    value.split(';').find_map(|piece| {
        let (name, text) = piece.trim().split_once('=')?;
        (name.trim() == key).then(|| text.trim().trim_matches('"').replace("%22", "\""))
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_with_a_text_field_and_a_file_is_read() {
        let body = Bytes::from_static(
            b"--XyZ\r\nContent-Disposition: form-data; name=\"parent\"\r\n\r\n1200000000000001\r\n\
              --XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\n\
              Content-Type: text/plain\r\n\r\nline one\r\nline two\r\n--XyZ--\r\n",
        );
        let boundary = boundary("multipart/form-data; boundary=XyZ").unwrap();
        let parts = parse(&body, &boundary).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "parent");
        assert_eq!(&parts[0].data[..], b"1200000000000001");
        assert_eq!(parts[1].file_name.as_deref(), Some("a.txt"));
        assert_eq!(parts[1].content_type.as_deref(), Some("text/plain"));
        assert_eq!(&parts[1].data[..], b"line one\r\nline two");
    }

    #[test]
    fn a_body_without_the_boundary_is_refused() {
        assert!(parse(&Bytes::from_static(b"nothing here"), "XyZ").is_none());
        assert!(boundary("application/json").is_none());
    }
}
