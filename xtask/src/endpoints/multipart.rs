//! The specification's one multipart operation, attachment upload, rendered from typed fields.
//!
//! `AttachmentRequest` is a loose form: `resource_subtype` says whether `file` (an upload) or
//! `url` and `name` (a link to a file kept elsewhere) are required. The descriptor instead takes an
//! `AttachmentContent` that is one or the other, and an `AttachmentParent` that is a typed task,
//! project or project brief GID, so an invalid combination cannot be built.

use openapiv3::{Schema, SchemaKind, Type};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use super::docs;
use super::operation::Operation;
use super::render::{self, Types};
use crate::GenResult;

/// The form fields this module knows how to fill. A change upstream fails generation here.
const FORM_FIELDS: &[&str] = &[
    "resource_subtype",
    "file",
    "parent",
    "url",
    "name",
    "connect_to_app",
];

/// The kinds of object an attachment's `parent` may name, as (variant, GID family).
const PARENTS: &[(&str, &str)] = &[
    ("Task", "Task"),
    ("Project", "Project"),
    ("ProjectBrief", "ProjectBrief"),
];

/// The descriptor of the multipart operation `operation`, whose form is `schema`.
pub fn descriptor(
    operation: &Operation<'_>,
    types: &Types,
    schema: &Schema,
    gid_families: &[String],
) -> GenResult<TokenStream> {
    let SchemaKind::Type(Type::Object(object)) = &schema.schema_kind else {
        return Err("the multipart form is not an object".to_owned());
    };
    let mut fields: Vec<&str> = object.properties.keys().map(String::as_str).collect();
    fields.sort_unstable();
    let mut expected = FORM_FIELDS.to_vec();
    expected.sort_unstable();
    if fields != expected {
        return Err(format!(
            "the attachment form's fields changed to {fields:?}: update xtask/src/endpoints/multipart.rs"
        ));
    }
    if !operation.path_params.is_empty() {
        return Err("the attachment upload gained a path parameter".to_owned());
    }
    for (_, family) in PARENTS {
        if !gid_families.iter().any(|known| known == family) {
            return Err(format!(
                "an attachment parent needs the GID type of {family}"
            ));
        }
    }

    let name = format_ident!("{}", operation.type_name);
    let doc = docs::attributes(&docs::operation(
        operation.summary.as_deref(),
        operation.description.as_deref(),
        operation.deprecated,
        operation.method.as_str(),
        &operation.route,
    ));
    let many = operation.returns_many();
    let opt_fields_const = render::opt_fields_const(operation);
    let query_fields = operation
        .query_params
        .iter()
        .map(|param| render::query_field(param, many));
    let query = render::query_fn(&operation.query_params);
    let output = &types.output;
    let operation_id = &operation.id;
    let route = &operation.route;
    let scope = match &types.scope {
        Some(variant) => quote! { Some(crate::scopes::Scope::#variant) },
        None => quote! { None },
    };
    let idempotent = types.idempotent;

    let parent_variants: Vec<_> = PARENTS
        .iter()
        .map(|(variant, _)| format_ident!("{variant}"))
        .collect();
    let parent_gids: Vec<_> = PARENTS
        .iter()
        .map(|(_, family)| format_ident!("{}", crate::naming::gid_type_name(family)))
        .collect();
    let parent_docs = PARENTS.iter().map(|(variant, _)| {
        format!(
            " A {}.",
            heck::AsSnakeCase(variant).to_string().replace('_', " ")
        )
    });

    Ok(quote! {
        /// Where an attachment is attached: a task, a project or a project brief.
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum AttachmentParent {
            #( #[doc = #parent_docs] #parent_variants(crate::ids::#parent_gids), )*
        }

        impl AttachmentParent {
            /// The parent's GID, as the form sends it.
            pub fn gid(&self) -> &str {
                match self {
                    #( Self::#parent_variants(gid) => crate::ids::Gid::as_str(gid), )*
                }
            }
        }

        #(
            impl From<crate::ids::#parent_gids> for AttachmentParent {
                fn from(gid: crate::ids::#parent_gids) -> Self {
                    Self::#parent_variants(gid)
                }
            }
        )*

        /// What an attachment holds: an uploaded file, or a link to a file kept elsewhere.
        #[derive(Clone)]
        #[non_exhaustive]
        pub enum AttachmentContent {
            /// A file uploaded to Asana (`resource_subtype: asana`).
            File {
                /// The name Asana shows for the file.
                file_name: String,
                /// The file's media type, for example `application/pdf`.
                content_type: String,
                /// The file's bytes.
                bytes: bytes::Bytes,
            },
            /// A link to a file kept elsewhere (`resource_subtype: external`).
            External {
                /// The file's address.
                url: String,
                /// The name Asana shows for the link.
                name: String,
            },
        }

        impl AttachmentContent {
            /// A file to upload.
            pub fn file(
                file_name: impl Into<String>,
                content_type: impl Into<String>,
                bytes: impl Into<bytes::Bytes>,
            ) -> Self {
                Self::File {
                    file_name: file_name.into(),
                    content_type: content_type.into(),
                    bytes: bytes.into(),
                }
            }

            /// A link to a file kept elsewhere.
            pub fn external(url: impl Into<String>, name: impl Into<String>) -> Self {
                Self::External {
                    url: url.into(),
                    name: name.into(),
                }
            }
        }

        /// Shows the file's media type and size, or the link's name; never the bytes or the URL.
        impl std::fmt::Debug for AttachmentContent {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::File { content_type, bytes, .. } => f
                        .debug_struct("File")
                        .field("content_type", content_type)
                        .field("len", &bytes.len())
                        .finish_non_exhaustive(),
                    Self::External { name, .. } => f
                        .debug_struct("External")
                        .field("name", name)
                        .finish_non_exhaustive(),
                }
            }
        }

        #doc
        #[derive(Clone, Debug, bon::Builder)]
        pub struct #name {
            /// The task, project or project brief to attach to.
            #[builder(start_fn, into)]
            pub parent: AttachmentParent,
            /// The file or link to attach.
            #[builder(start_fn)]
            pub content: AttachmentContent,
            /// Whether to connect the calling app to the attachment so it shows the app's widget.
            /// Only for a link on a task, and only with an OAuth token.
            pub connect_to_app: Option<bool>,
            #( #query_fields )*
        }

        impl crate::endpoint::Endpoint for #name {
            type Output = #output;

            const OPERATION_ID: &'static str = #operation_id;
            const METHOD: crate::endpoint::Method = crate::endpoint::Method::Post;
            const ROUTE: &'static str = #route;
            const SCOPE: Option<crate::scopes::Scope> = #scope;
            const IDEMPOTENT: bool = #idempotent;
            #opt_fields_const

            fn path(&self) -> String {
                #route.to_owned()
            }

            #query

            fn body(&self) -> Result<crate::endpoint::RequestBody, serde_json::Error> {
                let text = |name: &'static str, value: String| crate::endpoint::Part {
                    name: name.into(),
                    value: crate::endpoint::PartValue::Text(value),
                };
                let mut parts = vec![text("parent", self.parent.gid().to_owned())];
                match &self.content {
                    AttachmentContent::File { file_name, content_type, bytes } => {
                        parts.push(text("resource_subtype", "asana".to_owned()));
                        parts.push(crate::endpoint::Part {
                            name: "file".into(),
                            value: crate::endpoint::PartValue::File {
                                file_name: file_name.clone(),
                                content_type: content_type.clone(),
                                bytes: bytes.clone(),
                            },
                        });
                    }
                    AttachmentContent::External { url, name } => {
                        parts.push(text("resource_subtype", "external".to_owned()));
                        parts.push(text("url", url.clone()));
                        parts.push(text("name", name.clone()));
                    }
                }
                if let Some(connect) = self.connect_to_app {
                    parts.push(text("connect_to_app", connect.to_string()));
                }
                Ok(crate::endpoint::RequestBody::Multipart(parts))
            }
        }
    })
}
