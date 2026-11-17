//! The `opt_fields` enums: one fieldless enum per resource family, the union of the paths every
//! operation returning that family lists.
//!
//! An operation's family is the model its response's `data` (or each element of it) becomes, as
//! [`crate::naming::role_of`] names it. The models half asks [`field_families`] which families have
//! an enum, and implements `HasFields` for them so the `fields!` macro can find the enum.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

use crate::GenResult;
use crate::emit::GeneratedFile;
use crate::endpoints::operation::Operation;
use crate::endpoints::render::variant;
use crate::naming;
use crate::spec::Spec;

/// The families that have a generated `{Family}Field` enum, by the model type name
/// (`naming::role_of(..).type_name()`), for example `Task` or `CustomField`.
pub fn field_families(spec: &Spec) -> GenResult<BTreeSet<String>> {
    let operations = crate::endpoints::operations(spec)?;
    Ok(paths_by_family(&operations).into_keys().collect())
}

/// Every family's paths, sorted.
fn paths_by_family(operations: &[Operation<'_>]) -> BTreeMap<String, BTreeSet<String>> {
    let mut families: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for operation in operations {
        if let Some((family, paths)) = operation.opt_fields() {
            families
                .entry(family.to_owned())
                .or_default()
                .extend(paths.iter().cloned());
        }
    }
    families
}

/// Generates `fields/mod.rs` and one `fields/<family>.rs` per family.
pub fn generate(operations: &[Operation<'_>]) -> GenResult<Vec<GeneratedFile>> {
    let families = paths_by_family(operations);
    let mut files = Vec::new();
    let mut modules = Vec::new();
    for (family, paths) in &families {
        let module = format_ident!("{}", naming::module_name(family));
        let name = format_ident!("{}", naming::field_enum_name(family));
        files.push(GeneratedFile {
            path: format!("fields/{module}.rs").into(),
            tokens: field_enum(family, &name, paths)?,
        });
        modules.push((module, name));
    }
    let declarations = modules.iter().map(|(module, _)| quote! { mod #module; });
    let exports = modules
        .iter()
        .map(|(module, name)| quote! { pub use #module::#name; });
    files.push(GeneratedFile {
        path: "fields/mod.rs".into(),
        tokens: quote! {
            //! One enum per resource of the `opt_fields` paths Asana accepts for it.
            #( #declarations )*
            #( #exports )*
        },
    });
    Ok(files)
}

fn field_enum(family: &str, name: &Ident, paths: &BTreeSet<String>) -> GenResult<TokenStream> {
    let variants: Vec<Ident> = paths.iter().map(|path| variant(path)).collect();
    let mut seen = BTreeSet::new();
    for (path, variant) in paths.iter().zip(&variants) {
        if !seen.insert(variant.to_string()) {
            return Err(format!(
                "two {family} field paths become {variant}, one is {path}"
            ));
        }
    }
    let docs = paths.iter().map(|path| format!(" `{path}`"));
    let module_doc = format!(" The `opt_fields` paths of `{family}`.");
    let enum_doc = format!(
        " The `opt_fields` paths of `{family}`: the union of what every operation returning it lists."
    );
    Ok(quote! {
        #![doc = #module_doc]

        #[doc = #enum_doc]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum #name {
            #( #[doc = #docs] #variants, )*
        }

        impl #name {
            /// Every field, in declaration order.
            pub const ALL: &'static [Self] = &[ #( Self::#variants ),* ];

            /// Every field's path, in declaration order, so `PATHS[field as usize]` is its path.
            const PATHS: &'static [&'static str] = &[ #( #paths ),* ];

            /// The field for a path the specification lists, usable in a `const` context.
            pub const fn lookup(path: &str) -> Option<Self> {
                let mut index = 0;
                while index < Self::PATHS.len() {
                    if crate::fields::const_str_eq(path, Self::PATHS[index]) {
                        return Some(Self::ALL[index]);
                    }
                    index += 1;
                }
                None
            }
        }

        impl crate::fields::FieldPath for #name {
            fn path(self) -> &'static str {
                Self::PATHS[self as usize]
            }
        }
    })
}
