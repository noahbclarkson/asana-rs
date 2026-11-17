//! Splits the rendered items into files: one per family (with the requests that belong to it),
//! a shared file for the rest, the `models/mod.rs` that re-exports them all flat, `ids.rs`, and
//! the generated directory's `mod.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use heck::ToSnakeCase;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use super::helpers;
use super::ir::{Item, ItemKind};
use super::render::Renderer;
use super::{Analysis, Direction};
use crate::GenResult;
use crate::emit::GeneratedFile;

/// The most lines a generated file may have before its items are split across files.
const MAX_LINES: usize = 1_400;

/// The module that holds items with no family of their own.
const SHARED_MODULE: &str = "common";

/// Every file the models half writes.
pub fn files(
    analysis: &Analysis,
    field_families: &BTreeSet<String>,
) -> GenResult<Vec<GeneratedFile>> {
    let local = BTreeSet::new();
    let mut renderer = Renderer {
        field_families,
        local: &local,
        used: BTreeSet::new(),
    };
    let mut groups: BTreeMap<String, Vec<(&Item, TokenStream, usize)>> = BTreeMap::new();
    for item in analysis.items.values() {
        let tokens = renderer.item(item)?;
        let lines = count_lines(&tokens)?;
        let module = item
            .home
            .as_deref()
            .map_or_else(|| SHARED_MODULE.to_owned(), ToSnakeCase::to_snake_case);
        groups
            .entry(module)
            .or_default()
            .push((item, tokens, lines));
    }
    let mut files = Vec::new();
    let mut modules = Vec::new();
    for (module, mut items) in groups {
        items.sort_by_key(|(item, _, _)| order(item, &module));
        for (index, chunk) in chunks(items).into_iter().enumerate() {
            let name = if index == 0 {
                module.clone()
            } else {
                format!("{module}_{}", index + 1)
            };
            check_module_name(&name)?;
            let doc = module_doc(&module, chunk.first().map(|(item, _, _)| *item));
            let bodies = chunk.into_iter().map(|(_, tokens, _)| tokens);
            files.push(GeneratedFile {
                path: PathBuf::from(format!("models/{name}.rs")),
                tokens: quote! {
                    #![doc = #doc]
                    #(#bodies)*
                },
            });
            modules.push(name);
        }
    }
    let helpers = helpers::module(&renderer.used);
    if let Some(tokens) = &helpers {
        files.push(GeneratedFile {
            path: PathBuf::from("models/serde_helpers.rs"),
            tokens: tokens.clone(),
        });
    }
    files.push(GeneratedFile {
        path: PathBuf::from("models/clearable.rs"),
        tokens: helpers::clearable_module(),
    });
    files.push(models_mod(&modules, helpers.is_some()));
    files.push(ids(analysis));
    files.push(generated_mod());
    Ok(files)
}

/// The family's own struct first, then the other read items, then the requests, each by name.
fn order(item: &Item, module: &str) -> (u8, String) {
    let rank = if item.name.to_snake_case() == module {
        0
    } else {
        match item.kind {
            ItemKind::Struct {
                direction: Direction::Write,
                ..
            }
            | ItemKind::WriteUnion { .. } => 2,
            _ => 1,
        }
    };
    (rank, item.name.clone())
}

fn chunks<T>(items: Vec<(T, TokenStream, usize)>) -> Vec<Vec<(T, TokenStream, usize)>> {
    let mut chunks = vec![Vec::new()];
    let mut lines = 0;
    for entry in items {
        if lines + entry.2 > MAX_LINES && chunks.last().is_some_and(|chunk| !chunk.is_empty()) {
            chunks.push(Vec::new());
            lines = 0;
        }
        lines += entry.2 + 1;
        if let Some(chunk) = chunks.last_mut() {
            chunk.push(entry);
        }
    }
    chunks
}

fn count_lines(tokens: &TokenStream) -> GenResult<usize> {
    let file: syn::File = syn::parse2(tokens.clone())
        .map_err(|error| format!("a generated item is not valid Rust: {error}"))?;
    Ok(prettyplease::unparse(&file).lines().count())
}

fn check_module_name(name: &str) -> GenResult<()> {
    if syn::parse_str::<syn::Ident>(name).is_err() {
        return Err(format!("`{name}` cannot be a module name"));
    }
    Ok(())
}

fn module_doc(module: &str, first: Option<&Item>) -> String {
    match first {
        Some(item) if module != SHARED_MODULE => format!(
            " [`{}`](crate::models::{}) and the types and requests that go with it.",
            item.name, item.name
        ),
        _ => " Types that belong to no single resource.".to_owned(),
    }
}

fn models_mod(modules: &[String], has_helpers: bool) -> GeneratedFile {
    let declarations = modules.iter().map(|module| {
        let module = format_ident!("{module}");
        quote!(mod #module;)
    });
    let exports = modules.iter().map(|module| {
        let module = format_ident!("{module}");
        quote!(pub use #module::*;)
    });
    let helpers = has_helpers.then(|| {
        quote!(
            pub(crate) mod serde_helpers;
        )
    });
    GeneratedFile {
        path: PathBuf::from("models/mod.rs"),
        tokens: quote! {
            #![doc = " One type per Asana resource, and the request bodies, enums and unions around them."]
            #![doc = ""]
            #![doc = " A resource such as a task has several shapes in Asana's specification (`TaskCompact`,"]
            #![doc = " `TaskBase`, `TaskResponse`). Because `opt_fields` decides what a response holds, they are"]
            #![doc = " one struct here, [`Task`], with every field optional. Requests are types of their own,"]
            #![doc = " built with their `builder()`. Enums keep values Asana adds later as `Unknown`, and unions"]
            #![doc = " keep kinds they do not know as `Other`, so a response never fails to decode for those."]

            mod clearable;
            #helpers
            #(#declarations)*

            pub use clearable::Clearable;
            #(#exports)*
        },
    }
}

fn ids(analysis: &Analysis) -> GeneratedFile {
    let entries = analysis
        .classes
        .gid_families
        .iter()
        .map(|(family, resource_type)| {
            let name = format_ident!("{}", crate::naming::gid_type_name(family));
            quote!(#name => #resource_type)
        });
    GeneratedFile {
        path: PathBuf::from("ids.rs"),
        tokens: quote! {
            #![doc = " One GID type for every resource family that has a `gid`."]

            crate::gid!(#(#entries),*);
        },
    }
}

fn generated_mod() -> GeneratedFile {
    GeneratedFile {
        path: PathBuf::from("mod.rs"),
        tokens: quote! {
            #![doc = " Everything generated from Asana's specification by `cargo xtask gen`."]

            pub mod endpoints;
            pub(crate) mod fields;
            pub(crate) mod ids;
            pub mod models;
            pub mod scopes;
        },
    }
}
