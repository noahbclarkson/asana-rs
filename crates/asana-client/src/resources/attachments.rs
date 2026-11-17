//! Attachments: upload a file, link one kept elsewhere, list, read, download and delete.
//!
//! An upload is a `multipart/form-data` request, which the client writes from the generated
//! descriptor. A download is not an API call: Asana hands out a short-lived `download_url` on its
//! file storage, which [`Attachments::download`] fetches through the client's transport without
//! the token.
//!
//! ```no_run
//! # async fn run(client: asana_client::Client, task: asana_client::openapi::ids::TaskGid)
//! #     -> Result<(), asana_client::Error> {
//! use asana_client::resources::attachments::Upload;
//!
//! let pdf = bytes::Bytes::from_static(b"%PDF-1.7 ...");
//! let made = client
//!     .attachments()
//!     .upload(task, Upload::bytes("acme-plan.pdf", pdf, "application/pdf"))
//!     .send()
//!     .await?;
//! # let _ = made; Ok(()) }
//! ```

use asana_openapi::endpoints::{
    AttachmentContent, AttachmentParent, CreateAttachmentForObject, DeleteAttachment,
    GetAttachment, GetAttachmentsForObject,
};
use asana_openapi::fields::{AttachmentField, Fields};
use asana_openapi::ids::AttachmentGid;
use bytes::Bytes;
use http::header::{CONTENT_LENGTH, LOCATION, USER_AGENT};

use crate::resources::request::{Request, opt_fields_setter};
use crate::{Client, Error, Require, TransportError};

/// The largest download [`Attachments::download`] accepts unless told otherwise: 100 MiB, Asana's
/// own limit on an uploaded file.
pub const DEFAULT_MAX_DOWNLOAD: usize = 100 * 1024 * 1024;

/// How many redirects a download follows. Asana's `download_url` normally answers directly.
const MAX_REDIRECTS: usize = 3;

/// The name a download's errors carry, as an operation would.
const DOWNLOAD: &str = "downloadAttachment";

impl Client {
    /// Attachments: upload, link, list, read, download and delete.
    pub fn attachments(&self) -> Attachments<'_> {
        Attachments { client: self }
    }
}

/// The attachment operations, from [`Client::attachments`].
#[derive(Clone, Copy, Debug)]
pub struct Attachments<'a> {
    client: &'a Client,
}

opt_fields_setter!(
    CreateAttachmentForObject => AttachmentField,
    GetAttachmentsForObject => AttachmentField,
    GetAttachment => AttachmentField,
);

/// A file to upload: its name, its bytes and its media type.
#[derive(Clone)]
pub struct Upload {
    file_name: String,
    bytes: Bytes,
    content_type: String,
}

impl Upload {
    /// A file held in memory. `content_type` is its media type, for example `application/pdf`.
    pub fn bytes(
        file_name: impl Into<String>,
        bytes: impl Into<Bytes>,
        content_type: impl Into<String>,
    ) -> Self {
        Self {
            file_name: file_name.into(),
            bytes: bytes.into(),
            content_type: content_type.into(),
        }
    }
}

/// Shows the media type and size, never the bytes.
impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upload")
            .field("content_type", &self.content_type)
            .field("len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl From<Upload> for AttachmentContent {
    fn from(upload: Upload) -> Self {
        Self::file(upload.file_name, upload.content_type, upload.bytes)
    }
}

impl<'a> Attachments<'a> {
    /// Uploads a file to a task, project or project brief.
    ///
    /// Sends `createAttachmentForObject` (`POST /attachments`, `multipart/form-data`); scope
    /// `attachments:write`; never retried after a failure that leaves the outcome unknown,
    /// because a second send would attach the file twice. The batch API cannot carry an upload.
    pub fn upload(
        &self,
        parent: impl Into<AttachmentParent>,
        file: Upload,
    ) -> Request<'a, CreateAttachmentForObject> {
        Request::new(
            self.client,
            CreateAttachmentForObject::builder(parent, file.into()).build(),
        )
    }

    /// Attaches a link to a file kept elsewhere, shown under `name`.
    ///
    /// Sends `createAttachmentForObject` (`POST /attachments`, `resource_subtype=external`);
    /// scope `attachments:write`; never retried after a failure that leaves the outcome unknown.
    pub fn attach_url(
        &self,
        parent: impl Into<AttachmentParent>,
        url: impl Into<String>,
        name: impl Into<String>,
    ) -> Request<'a, CreateAttachmentForObject> {
        Request::new(
            self.client,
            CreateAttachmentForObject::builder(parent, AttachmentContent::external(url, name))
                .build(),
        )
    }

    /// The attachments on a task, project or project brief, as a list.
    ///
    /// Sends `getAttachmentsForObject` (`GET /attachments?parent=…`); scope `attachments:read`;
    /// retried like every read.
    pub fn list(
        &self,
        parent: impl Into<AttachmentParent>,
    ) -> Request<'a, GetAttachmentsForObject> {
        let parent = parent.into();
        Request::new(
            self.client,
            GetAttachmentsForObject::builder()
                .parent(parent.gid())
                .build(),
        )
    }

    /// One attachment.
    ///
    /// Sends `getAttachment` (`GET /attachments/{attachment_gid}`); scope `attachments:read`;
    /// retried like every read.
    pub fn get(&self, attachment: AttachmentGid) -> Request<'a, GetAttachment> {
        Request::new(self.client, GetAttachment::builder(attachment).build())
    }

    /// Deletes an attachment.
    ///
    /// Sends `deleteAttachment` (`DELETE /attachments/{attachment_gid}`); scope
    /// `attachments:delete`; retried on a server failure, and a `404` to a retry after an attempt
    /// whose answer was lost counts as deleted.
    pub fn delete(&self, attachment: AttachmentGid) -> Request<'a, DeleteAttachment> {
        Request::new(self.client, DeleteAttachment::builder(attachment).build())
    }

    /// Downloads an uploaded file's bytes.
    ///
    /// Reads the attachment's `download_url` with `getAttachment` (scope `attachments:read`,
    /// retried like every read), then fetches that URL once, not retried: it is short-lived, so
    /// on a failure read the attachment again for a fresh one. A link to a file kept elsewhere
    /// has no `download_url`.
    ///
    /// The URL is on Asana's file storage, not the API, so:
    /// - the token is never sent to it, and the client's rate limits do not apply;
    /// - it must be `https`, and so must any redirect, of which at most three are followed: a
    ///   redirect to plain `http` or to an address with a user name is refused rather than
    ///   followed, so the bytes cannot be steered somewhere unexpected;
    /// - a body over [`max_bytes`](Download::max_bytes) (100 MiB unless set) is refused. The
    ///   transport reads a body whole before answering, so its own body cap bounds memory
    ///   ([`ReqwestTransport`](crate::ReqwestTransport): 16 MiB by default); raise both for
    ///   large files.
    pub fn download(&self, attachment: AttachmentGid) -> Download<'a> {
        Download {
            client: self.client,
            source: Source::Attachment(attachment),
            max_bytes: DEFAULT_MAX_DOWNLOAD,
        }
    }

    /// Downloads from a `download_url` the caller has already read, as
    /// [`download`](Self::download) does after reading it.
    pub fn download_url(&self, url: impl Into<String>) -> Download<'a> {
        Download {
            client: self.client,
            source: Source::Url(url.into()),
            max_bytes: DEFAULT_MAX_DOWNLOAD,
        }
    }
}

/// Where a download starts.
#[derive(Debug)]
enum Source {
    Attachment(AttachmentGid),
    Url(String),
}

/// The request [`Attachments::download`] returns.
#[must_use = "a request does nothing until it is sent"]
#[derive(Debug)]
pub struct Download<'a> {
    client: &'a Client,
    source: Source,
    max_bytes: usize,
}

impl Download<'_> {
    /// The largest body accepted, in bytes.
    pub fn max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Fetches the file and returns its bytes.
    ///
    /// # Errors
    ///
    /// [`Error::MissingField`] when the attachment has no `download_url`;
    /// [`Error::InvalidRequest`] when the URL or a redirect is not `https` or there are too many
    /// redirects; [`Error::Transport`] with a body-too-large error when the file is over the
    /// cap, or when the storage could not be reached; the status's error (an expired URL answers
    /// `403`) for any other answer.
    pub async fn send(self) -> Result<Bytes, Error> {
        let url = match self.source {
            Source::Url(url) => url,
            Source::Attachment(gid) => {
                let attachment = self
                    .client
                    .send(
                        GetAttachment::builder(gid)
                            .opt_fields(Fields::none().with(AttachmentField::DownloadUrl))
                            .build(),
                    )
                    .await?;
                attachment.download_url.require("download_url")?
            }
        };
        fetch(self.client, &url, self.max_bytes).await
    }
}

async fn fetch(client: &Client, url: &str, max_bytes: usize) -> Result<Bytes, Error> {
    let mut current = checked_url(url::Url::parse(url).ok())?;
    let transport = client.transport();
    for _ in 0..=MAX_REDIRECTS {
        let request = http::Request::get(current.as_str())
            .header(USER_AGENT, client.user_agent().clone())
            .body(Bytes::new())
            .map_err(|_| refused("its download URL is not a valid request target"))?;
        let response = transport
            .send(request)
            .await
            .map_err(|source| Error::Transport {
                source,
                attempts: 1,
                safe_to_repeat: true,
            })?;
        let status = response.status();
        if status.is_redirection() {
            let next = response
                .headers()
                .get(LOCATION)
                .and_then(|location| location.to_str().ok())
                .and_then(|location| current.join(location).ok());
            current = checked_url(next)?;
            continue;
        }
        if !status.is_success() {
            return Err(Error::from_status(status, response.body(), 1, true));
        }
        let declared = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|length| length.to_str().ok())
            .and_then(|length| length.parse::<usize>().ok());
        let body = response.into_body();
        if declared.is_some_and(|length| length > max_bytes) || body.len() > max_bytes {
            return Err(Error::Transport {
                source: TransportError::body_too_large(max_bytes),
                attempts: 1,
                safe_to_repeat: true,
            });
        }
        return Ok(body);
    }
    Err(refused("its download redirected too many times"))
}

/// Accepts only an `https` URL with no user name or password.
fn checked_url(url: Option<url::Url>) -> Result<url::Url, Error> {
    match url {
        Some(url)
            if url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.host().is_some() =>
        {
            Ok(url)
        }
        Some(_) => Err(refused(
            "its download URL or a redirect is not a plain https address",
        )),
        None => Err(refused("its download URL or a redirect is not a valid URL")),
    }
}

fn refused(reason: &'static str) -> Error {
    Error::InvalidRequest {
        operation: DOWNLOAD,
        reason,
    }
}
