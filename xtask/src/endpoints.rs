//! The endpoints half: operations to descriptors, the `opt_fields` enums and the scope table.
//!
//! Every operation becomes a descriptor in the module of its tag (`endpoints/tasks.rs`), split into
//! several files when one would pass [`MAX_LINES`], and is re-exported flat from `endpoints`.

pub mod docs;
mod multipart;
pub mod operation;
pub mod render;
mod support;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use heck::ToSnakeCase;
use openapiv3::{ReferenceOr, Schema};
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

use crate::GenResult;
use crate::emit::GeneratedFile;
use crate::models::{self, Direction, TypeRef};
use crate::spec::Spec;
use operation::{Body, Data, Method, Operation, ParamKind, ResponseShape};

/// The most lines a generated module may hold before it is split.
const MAX_LINES: usize = 1_400;

/// The `POST` operations that are idempotent in effect: sending one twice leaves Asana as sending
/// it once would, so the client may resend it after a failure that leaves the outcome unknown.
/// Every other `POST` creates something or starts a job, and is never resent.
const IDEMPOTENT_POSTS: &[&str] = &[
    // Tags on a task.
    "addTagForTask",
    "removeTagForTask",
    // Followers of a task, project or goal.
    "addFollowersForTask",
    "removeFollowerForTask",
    "addFollowersForProject",
    "removeFollowersForProject",
    "addFollowers",
    "removeFollowers",
    // A task's projects, and moving it into a section or under a parent.
    "addProjectForTask",
    "removeProjectForTask",
    "addTaskForSection",
    "setParentForTask",
    // Dependencies and dependents of a task.
    "addDependenciesForTask",
    "removeDependenciesForTask",
    "addDependentsForTask",
    "removeDependentsForTask",
    // Members of a project, portfolio, team or workspace, and a portfolio's items.
    "addMembersForProject",
    "removeMembersForProject",
    "addMembersForPortfolio",
    "removeMembersForPortfolio",
    "addItemForPortfolio",
    "removeItemForPortfolio",
    "addUserForTeam",
    "removeUserForTeam",
    "addUserForWorkspace",
    "removeUserForWorkspace",
];

/// Every operation in the specification, read and typed.
pub fn operations(spec: &Spec) -> GenResult<Vec<Operation<'_>>> {
    operation::collect(spec, &gid_families(spec)?)
}

/// The families with a typed GID, from the models half.
fn gid_families(spec: &Spec) -> GenResult<Vec<String>> {
    models::gid_families(spec)
}

/// Generates `endpoints/**`, `fields/**` and `scopes.rs`.
pub fn generate(spec: &Spec) -> GenResult<Vec<GeneratedFile>> {
    let families = gid_families(spec)?;
    let operations = operation::collect(spec, &families)?;
    let known_scopes: Vec<String> = crate::scopes::scopes(spec)?
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    for id in IDEMPOTENT_POSTS {
        if !operations
            .iter()
            .any(|operation| operation.id == *id && operation.method == Method::Post)
        {
            return Err(format!(
                "IDEMPOTENT_POSTS names {id}, which is not a POST operation"
            ));
        }
    }

    let mut modules: BTreeMap<String, Vec<TokenStream>> = BTreeMap::new();
    let mut wholes: BTreeMap<&'static str, TokenStream> = BTreeMap::new();
    let mut helpers = support::Helpers::default();
    let mut rows = Vec::new();
    for operation in &operations {
        let rendered = descriptor(spec, operation, &families, &known_scopes, &mut wholes)
            .map_err(|error| format!("{}: {error}", operation.id))?;
        helpers.note(&rendered);
        let module = operation.tag.to_snake_case();
        rows.push((module.clone(), operation));
        modules.entry(module).or_default().push(rendered);
    }

    let mut files = Vec::new();
    let mut declarations = Vec::new();
    let mut exports = Vec::new();
    let mut module_of: BTreeMap<String, String> = BTreeMap::new();
    for (module, items) in modules {
        for (file_module, tokens) in split(&module, items)? {
            let file_ident = format_ident!("{file_module}");
            let mut names = public_names(&tokens)?;
            names.sort();
            for name in &names {
                module_of.insert(name.to_string(), file_module.clone());
            }
            declarations.push(quote! { mod #file_ident; });
            exports.push(quote! { pub use #file_ident::{ #( #names ),* }; });
            files.push(GeneratedFile {
                path: format!("endpoints/{file_module}.rs").into(),
                tokens,
            });
        }
    }

    let index_rows: Vec<support::IndexRow> = rows
        .iter()
        .map(|(_, operation)| {
            Ok(support::IndexRow {
                module: module_of
                    .get(&operation.type_name)
                    .cloned()
                    .ok_or_else(|| format!("{} was not placed in a module", operation.type_name))?,
                type_name: operation.type_name.clone(),
                tag: operation.tag.clone(),
                paginated: operation.paginated(),
            })
        })
        .collect::<GenResult<_>>()?;
    files.push(GeneratedFile {
        path: "endpoints/index.rs".into(),
        tokens: support::index(&index_rows),
    });

    let wholes = wholes.values();
    let helpers = helpers.items();
    files.push(GeneratedFile {
        path: "endpoints/mod.rs".into(),
        tokens: quote! {
            //! One descriptor per operation in Asana's specification, in a module per tag and
            //! re-exported here: `asana_openapi::endpoints::GetTask`.
            //!
            //! A descriptor is the operation's parameters as a `bon` builder, with the path's GIDs
            //! as the builder's arguments. It implements [`Endpoint`](crate::endpoint::Endpoint),
            //! and [`Paginated`](crate::endpoint::Paginated) when it returns pages.

            mod index;
            #( #declarations )*

            pub use index::{OPERATIONS, OperationInfo};
            #( #exports )*

            #( #wholes )*
            #helpers
        },
    });
    files.extend(crate::fields::generate(&operations)?);
    files.push(crate::scopes::generate(spec)?);
    Ok(files)
}

/// One operation's items.
fn descriptor(
    spec: &Spec,
    operation: &Operation<'_>,
    families: &[String],
    known_scopes: &[String],
    wholes: &mut BTreeMap<&'static str, TokenStream>,
) -> GenResult<TokenStream> {
    let mut extra: Vec<TokenStream> = Vec::new();
    let hint = &operation.type_name;
    let (output, item, whole) = match &operation.response {
        ResponseShape::Data(Data::One(schema)) => {
            let ty = resolve(spec, schema, &format!("{hint}Data"), Direction::Read)?;
            extra.extend(ty.items);
            (ty.ty, None, false)
        }
        ResponseShape::Data(Data::Many(schema)) => {
            let ty = resolve(spec, schema, &format!("{hint}Item"), Direction::Read)?;
            extra.extend(ty.items);
            let element = ty.ty;
            (quote! { Vec<#element> }, Some(element), false)
        }
        ResponseShape::Whole { siblings, data } => {
            let name = support::whole_type(siblings)?;
            if !wholes.contains_key(name) {
                let element = resolve(
                    spec,
                    data.element(),
                    &format!("{name}Data"),
                    Direction::Read,
                )?;
                let mut tokens = support::whole_item(name, &element.ty)?;
                tokens.extend(element.items);
                wholes.insert(name, tokens);
            }
            let name = format_ident!("{name}");
            (quote! { super::#name }, None, true)
        }
    };
    if operation.paginated() {
        let optional = |kind: ParamKind| {
            operation
                .query_params
                .iter()
                .any(|param| param.kind == kind && !param.required)
        };
        if !optional(ParamKind::Offset) {
            return Err("a paged list needs an optional offset parameter".to_owned());
        }
    }
    let body = match &operation.body {
        Body::Json { data, .. } => {
            let ty = resolve(spec, data, &format!("{hint}Body"), Direction::Write)?;
            extra.extend(ty.items);
            Some(ty.ty)
        }
        Body::None | Body::Multipart(_) => None,
    };
    let scope = match &operation.scope {
        Some(scope) if known_scopes.contains(scope) => Some(crate::scopes::variant(scope)),
        Some(scope) => return Err(format!("the scope {scope} is not in the oauth2 scheme")),
        None => None,
    };
    let types = render::Types {
        output,
        item,
        body,
        whole,
        idempotent: operation.method != Method::Post
            || IDEMPOTENT_POSTS.contains(&operation.id.as_str()),
        scope,
    };
    let mut tokens = match &operation.body {
        Body::Multipart(schema) => multipart::descriptor(operation, &types, schema, families)?,
        _ => render::descriptor(operation, &types)?,
    };
    tokens.extend(extra);
    Ok(tokens)
}

/// The Rust type for a schema in an operation, from the models half.
fn resolve(
    spec: &Spec,
    schema: &ReferenceOr<Schema>,
    hint: &str,
    direction: Direction,
) -> GenResult<TypeRef> {
    models::rust_type(spec, schema, hint, direction)
}

/// Splits a module's descriptors into files of at most [`MAX_LINES`] formatted lines: `tasks`,
/// then `tasks_2`, `tasks_3`, and so on.
fn split(module: &str, items: Vec<TokenStream>) -> GenResult<Vec<(String, TokenStream)>> {
    let mut chunks: Vec<(usize, Vec<TokenStream>)> = vec![(0, Vec::new())];
    for item in items {
        let lines = formatted_lines(&item)? + 1;
        if let Some((used, chunk)) = chunks.last_mut()
            && (chunk.is_empty() || *used + lines <= MAX_LINES)
        {
            *used += lines;
            chunk.push(item);
            continue;
        }
        chunks.push((lines, vec![item]));
    }
    let count = chunks.len();
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, (_, items))| {
            let name = if index == 0 {
                module.to_owned()
            } else {
                format!("{module}_{}", index + 1)
            };
            let doc = if count == 1 {
                format!(" The operations Asana's specification tags `{module}`.")
            } else {
                format!(
                    " The operations Asana's specification tags `{module}`, part {} of {count}.",
                    index + 1
                )
            };
            (name, quote! { #![doc = #doc] #( #items )* })
        })
        .collect())
}

fn formatted_lines(tokens: &TokenStream) -> GenResult<usize> {
    let file: syn::File = syn::parse2(tokens.clone())
        .map_err(|error| format!("a descriptor is not valid Rust: {error}"))?;
    Ok(prettyplease::unparse(&file).lines().count())
}

/// The names of the public items in a module, to re-export.
fn public_names(tokens: &TokenStream) -> GenResult<Vec<Ident>> {
    let file: syn::File = syn::parse2(tokens.clone())
        .map_err(|error| format!("a module is not valid Rust: {error}"))?;
    Ok(file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Struct(item) if is_pub(&item.vis) => Some(item.ident.clone()),
            syn::Item::Enum(item) if is_pub(&item.vis) => Some(item.ident.clone()),
            syn::Item::Type(item) if is_pub(&item.vis) => Some(item.ident.clone()),
            _ => None,
        })
        .collect())
}

fn is_pub(visibility: &syn::Visibility) -> bool {
    matches!(visibility, syn::Visibility::Public(_))
}
