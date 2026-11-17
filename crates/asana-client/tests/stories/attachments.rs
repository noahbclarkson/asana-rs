//! Uploading through the multipart form, and downloading from the storage URL.

use asana_client::openapi::ids::TaskGid;
use asana_client::resources::attachments::Upload;
use asana_client::{Error, TransportErrorKind};
use bytes::Bytes;
use serde_json::json;

use crate::support::{BASE, Scripted, TASK, client, reply, reply_with};
use crate::{ATTACHMENT, gid};

const FILES: &str = "https://files.example.com/acme/plan.pdf?X-Amz-Signature=made-up";

fn attachment_body(download_url: &str) -> String {
    json!({ "data": {
        "gid": ATTACHMENT,
        "resource_type": "attachment",
        "name": "acme-plan.pdf",
        "resource_subtype": "asana",
        "download_url": download_url,
    }})
    .to_string()
}

fn file(status: u16, bytes: &'static [u8]) -> http::Response<Bytes> {
    http::Response::builder()
        .status(status)
        .header("content-type", "application/pdf")
        .body(Bytes::from_static(bytes))
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn an_upload_sends_parent_subtype_and_file_as_form_parts() {
    let made = attachment_body(FILES);
    let transport = Scripted::new(move |_, _| Ok(reply(200, &made)));
    let client = client(transport.clone());
    client
        .attachments()
        .upload(
            gid::<TaskGid>(TASK),
            Upload::bytes(
                "acme \"plan\".pdf",
                &b"%PDF-1.7 made up"[..],
                "application/pdf",
            ),
        )
        .send()
        .await
        .unwrap();

    let request = transport.request(0);
    assert_eq!(request.method(), http::Method::POST);
    assert_eq!(request.uri().to_string(), format!("{BASE}/attachments"));
    let content_type = request.headers()["content-type"].to_str().unwrap();
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    let body = std::str::from_utf8(request.body()).unwrap();
    let parts: Vec<&str> = body
        .split(&format!("--{boundary}"))
        .filter(|part| !part.is_empty() && *part != "--\r\n")
        .collect();
    assert_eq!(
        parts,
        [
            format!("\r\nContent-Disposition: form-data; name=\"parent\"\r\n\r\n{TASK}\r\n"),
            "\r\nContent-Disposition: form-data; name=\"resource_subtype\"\r\n\r\nasana\r\n"
                .to_owned(),
            "\r\nContent-Disposition: form-data; name=\"file\"; filename=\"acme %22plan%22.pdf\"\r\n\
             Content-Type: application/pdf\r\n\r\n%PDF-1.7 made up\r\n"
                .to_owned(),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_download_fetches_the_storage_url_without_the_token() {
    let meta = attachment_body(FILES);
    let transport = Scripted::new(move |request, _| {
        if request.uri().to_string().starts_with(BASE) {
            Ok(reply(200, &meta))
        } else {
            Ok(file(200, b"%PDF-1.7 the file"))
        }
    });
    let client = client(transport.clone());
    let bytes = client
        .attachments()
        .download(gid(ATTACHMENT))
        .send()
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), b"%PDF-1.7 the file");

    let read = transport.request(0);
    assert!(
        read.uri()
            .query()
            .unwrap()
            .contains("opt_fields=download_url")
    );
    assert!(read.headers().contains_key("authorization"));
    let fetch = transport.request(1);
    assert_eq!(fetch.uri().to_string(), FILES);
    assert!(!fetch.headers().contains_key("authorization"));
}

#[tokio::test(start_paused = true)]
async fn a_download_over_the_cap_is_refused() {
    let transport = Scripted::new(|_, _| Ok(file(200, b"0123456789ABCDEF")));
    let client = client(transport.clone());
    let error = client
        .attachments()
        .download_url(FILES)
        .max_bytes(10)
        .send()
        .await
        .unwrap_err();
    match error {
        Error::Transport { source, .. } => {
            assert_eq!(
                source.kind(),
                TransportErrorKind::BodyTooLarge { limit: 10 }
            );
        }
        other => panic!("expected a body-too-large error, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_redirect_is_followed_only_to_https() {
    let transport = Scripted::new(|request, _| {
        Ok(if request.uri().host() == Some("files.example.com") {
            reply_with(302, ("location", "https://cdn.example.com/plan.pdf"), "")
        } else {
            file(200, b"moved")
        })
    });
    let client = client(transport.clone());
    let bytes = client
        .attachments()
        .download_url(FILES)
        .send()
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), b"moved");
    assert_eq!(transport.count(), 2);

    let transport = Scripted::new(|_, _| {
        Ok(reply_with(
            302,
            ("location", "http://cdn.example.com/x"),
            "",
        ))
    });
    let client = crate::support::client(transport.clone());
    let error = client
        .attachments()
        .download_url(FILES)
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::InvalidRequest { .. }), "{error:?}");
    assert_eq!(transport.count(), 1);

    let error = client
        .attachments()
        .download_url("http://files.example.com/plan.pdf")
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::InvalidRequest { .. }));
    assert_eq!(transport.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_external_link_has_no_download() {
    let transport = Scripted::new(|_, _| {
        Ok(reply(
            200,
            &json!({ "data": { "gid": ATTACHMENT, "resource_subtype": "external", "download_url": null } })
                .to_string(),
        ))
    });
    let client = client(transport);
    let error = client
        .attachments()
        .download(gid(ATTACHMENT))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::MissingField("download_url")));
}
