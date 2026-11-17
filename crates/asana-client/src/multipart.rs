//! Writes a `multipart/form-data` body (RFC 7578), for uploading attachments.
//!
//! The form is small and wholly in memory, so it is written directly rather than through a
//! streaming library: each part is a boundary line, a `Content-Disposition` header, a
//! `Content-Type` for a file, a blank line and the value.

use asana_openapi::endpoint::{Part, PartValue};
use bytes::{BufMut, Bytes, BytesMut};
use rand::Rng;
use rand::distr::Alphanumeric;

/// A form's body and the `Content-Type` header that names its boundary.
pub(crate) struct Form {
    pub(crate) content_type: String,
    pub(crate) body: Bytes,
}

/// Encodes `parts` with a fresh random boundary.
pub(crate) fn encode(parts: &[Part]) -> Form {
    let boundary: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(40)
        .map(char::from)
        .collect();
    encode_with_boundary(parts, &boundary)
}

fn encode_with_boundary(parts: &[Part], boundary: &str) -> Form {
    let size: usize = parts
        .iter()
        .map(|part| match &part.value {
            PartValue::Text(text) => text.len(),
            PartValue::File { bytes, .. } => bytes.len(),
        })
        .sum();
    let mut body = BytesMut::with_capacity(size + 256 * (parts.len() + 1));
    for part in parts {
        body.put_slice(b"--");
        body.put_slice(boundary.as_bytes());
        body.put_slice(b"\r\nContent-Disposition: form-data; name=\"");
        body.put_slice(quoted(&part.name).as_bytes());
        body.put_slice(b"\"");
        match &part.value {
            PartValue::Text(text) => {
                body.put_slice(b"\r\n\r\n");
                body.put_slice(text.as_bytes());
            }
            PartValue::File {
                file_name,
                content_type,
                bytes,
            } => {
                body.put_slice(b"; filename=\"");
                body.put_slice(quoted(file_name).as_bytes());
                body.put_slice(b"\"\r\nContent-Type: ");
                body.put_slice(header_safe(content_type).as_bytes());
                body.put_slice(b"\r\n\r\n");
                body.put_slice(bytes);
            }
        }
        body.put_slice(b"\r\n");
    }
    body.put_slice(b"--");
    body.put_slice(boundary.as_bytes());
    body.put_slice(b"--\r\n");
    Form {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        body: body.freeze(),
    }
}

/// Escapes a name for a quoted header parameter as browsers do (WHATWG's form encoding): `"`,
/// CR and LF are percent-encoded, so a file name cannot end the header or start another.
fn quoted(text: &str) -> String {
    text.replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Drops the characters that would end a header line from a media type.
fn header_safe(text: &str) -> String {
    text.chars()
        .filter(|character| !matches!(character, '\r' | '\n'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_has_one_delimited_part_per_field_and_a_closing_boundary() {
        let parts = vec![
            Part {
                name: "parent".into(),
                value: PartValue::Text("1201234567890123".into()),
            },
            Part {
                name: "file".into(),
                value: PartValue::File {
                    file_name: "Acme \"report\".pdf".into(),
                    content_type: "application/pdf\r\nX-Evil: 1".into(),
                    bytes: Bytes::from_static(b"%PDF-1.7\r\n\x00\xff"),
                },
            },
        ];
        let form = encode_with_boundary(&parts, "BOUNDARY");
        assert_eq!(form.content_type, "multipart/form-data; boundary=BOUNDARY");
        let expected: &[u8] = b"--BOUNDARY\r\n\
Content-Disposition: form-data; name=\"parent\"\r\n\r\n\
1201234567890123\r\n\
--BOUNDARY\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"Acme %22report%22.pdf\"\r\n\
Content-Type: application/pdfX-Evil: 1\r\n\r\n\
%PDF-1.7\r\n\x00\xff\r\n\
--BOUNDARY--\r\n";
        assert_eq!(form.body.as_ref(), expected);
    }

    #[test]
    fn each_form_gets_a_fresh_boundary() {
        let first = encode(&[]);
        let second = encode(&[]);
        assert_ne!(first.content_type, second.content_type);
        assert!(first.body.ends_with(b"--\r\n"));
    }
}
