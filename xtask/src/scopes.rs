//! The OAuth scope table: every scope in the specification's `oauth2` security scheme becomes a
//! variant of `asana_openapi::scopes::Scope`.

use heck::ToUpperCamelCase;
use openapiv3::{ReferenceOr, SecurityScheme};
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

use crate::GenResult;
use crate::emit::GeneratedFile;
use crate::spec::Spec;

/// One scope: its name as Asana writes it and the scheme's description of it.
pub struct ScopeEntry {
    pub name: String,
    pub description: String,
}

/// The scopes of the authorization-code flow, in specification order.
pub fn scopes(spec: &Spec) -> GenResult<Vec<ScopeEntry>> {
    let scheme = spec
        .api
        .components
        .as_ref()
        .and_then(|components| components.security_schemes.get("oauth2"))
        .ok_or("the specification has no oauth2 security scheme")?;
    let ReferenceOr::Item(SecurityScheme::OAuth2 { flows, .. }) = scheme else {
        return Err("the oauth2 security scheme is not an inline OAuth 2.0 scheme".to_owned());
    };
    let flow = flows
        .authorization_code
        .as_ref()
        .ok_or("the oauth2 scheme has no authorization-code flow")?;
    let mut entries: Vec<ScopeEntry> = flow
        .scopes
        .iter()
        .map(|(name, description)| ScopeEntry {
            name: name.clone(),
            description: description.clone(),
        })
        .collect();
    // Some operations name a scope the scheme's list leaves out (`goals:write`, for one). Each
    // descriptor's `SCOPE` must be a variant, so those join the enum after the listed ones.
    let mut unlisted = std::collections::BTreeSet::new();
    for item in spec.api.paths.paths.values() {
        let ReferenceOr::Item(item) = item else {
            continue;
        };
        for (_, operation) in item.iter() {
            for requirement in operation.security.iter().flatten() {
                for scope in requirement.get("oauth2").into_iter().flatten() {
                    if !entries.iter().any(|entry| &entry.name == scope) {
                        unlisted.insert(scope.clone());
                    }
                }
            }
        }
    }
    entries.extend(unlisted.into_iter().map(|name| ScopeEntry {
        name,
        description:
            "Named by an operation but missing from the specification's list of scopes.".to_owned(),
    }));
    let mut variants = std::collections::BTreeSet::new();
    for entry in &entries {
        if !variants.insert(variant_name(&entry.name)) {
            return Err(format!("two scopes become the variant of {}", entry.name));
        }
    }
    Ok(entries)
}

/// The variant for a scope: `tasks:read` gives `TasksRead`.
pub fn variant_name(scope: &str) -> String {
    scope.replace(':', "_").to_upper_camel_case()
}

/// The variant for a scope, as an identifier.
pub fn variant(scope: &str) -> Ident {
    format_ident!("{}", variant_name(scope))
}

/// Generates `scopes.rs`.
pub fn generate(spec: &Spec) -> GenResult<GeneratedFile> {
    let entries = scopes(spec)?;
    let variants: Vec<Ident> = entries.iter().map(|entry| variant(&entry.name)).collect();
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    let docs = entries.iter().map(|entry| {
        let lines = match crate::endpoints::docs::first_sentence(&entry.description) {
            Some(sentence) => vec![format!("`{}`: {sentence}", entry.name)],
            None => vec![format!("`{}`.", entry.name)],
        };
        crate::endpoints::docs::attributes(&lines)
    });
    let tokens: TokenStream = quote! {
        //! The OAuth scopes Asana's specification lists, and which one each operation needs (each
        //! descriptor's `Endpoint::SCOPE`).

        /// A granular OAuth scope an app requests when a user authorises it.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[non_exhaustive]
        pub enum Scope {
            #( #docs #variants, )*
        }

        impl Scope {
            /// Every scope: those the specification lists, then those only operations name.
            pub const ALL: &'static [Self] = &[ #( Self::#variants ),* ];

            /// The scope as Asana writes it, for example `tasks:read`.
            pub const fn as_str(self) -> &'static str {
                match self {
                    #( Self::#variants => #names, )*
                }
            }
        }

        impl std::fmt::Display for Scope {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        /// A piece of text that is not one of the OAuth scopes this crate knows.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
        #[error("not an Asana OAuth scope this crate knows")]
        #[non_exhaustive]
        pub struct UnknownScope;

        impl std::str::FromStr for Scope {
            type Err = UnknownScope;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                match text {
                    #( #names => Ok(Self::#variants), )*
                    _ => Err(UnknownScope),
                }
            }
        }
    };
    Ok(GeneratedFile {
        path: "scopes.rs".into(),
        tokens,
    })
}
