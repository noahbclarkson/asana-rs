//! Turns items into Rust tokens.

use std::collections::BTreeSet;

use heck::{ToSnakeCase, ToUpperCamelCase};
use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};

use super::Direction;
use super::helpers::Helper;
use super::ir::{Field, Item, ItemKind, Ty, UnionVariant};
use crate::GenResult;

/// Enums with this many values or more get functions long enough for `too_many_lines`.
const LONG_ENUM: usize = 110;

/// Rust keywords that a raw identifier can stand for.
const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn",
    "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let",
    "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return",
    "static", "struct", "trait", "true", "try", "type", "typeof", "unsized", "use", "virtual",
    "where", "while", "yield",
];

/// The Rust field name for a JSON key: snake case, a raw identifier for a keyword.
pub fn field_name(json: &str) -> String {
    let snake = json.replace(['.', '-', ' '], "_").to_snake_case();
    match snake.as_str() {
        "self" | "super" | "crate" | "" => format!("{snake}_"),
        keyword if KEYWORDS.contains(&keyword) => format!("r#{snake}"),
        _ if snake.starts_with(|c: char| c.is_ascii_digit()) => format!("_{snake}"),
        _ => snake,
    }
}

/// The variant name for an enum value.
pub fn variant_name(value: &str) -> String {
    let words: String = value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect();
    let name = words.to_upper_camel_case();
    if name.is_empty() {
        "Empty".to_owned()
    } else if name == "Unknown" {
        "UnknownValue".to_owned()
    } else if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("V{name}")
    } else {
        name
    }
}

fn ident(name: &str) -> Ident {
    match name.strip_prefix("r#") {
        Some(raw) => Ident::new_raw(raw, Span::call_site()),
        None => Ident::new(name, Span::call_site()),
    }
}

fn doc(text: &str) -> TokenStream {
    let text = format!(" {text}");
    quote!(#[doc = #text])
}

/// The tokens of a type, as written anywhere in `asana_openapi`. A model named in `local` is
/// written bare, because it is defined beside the code that uses it rather than in `models`.
pub fn ty_tokens(ty: &Ty, local: &BTreeSet<String>) -> TokenStream {
    match ty {
        Ty::Bool => quote!(bool),
        Ty::Integer => quote!(i64),
        Ty::Decimal => quote!(rust_decimal::Decimal),
        Ty::String => quote!(String),
        Ty::Date => quote!(time::Date),
        Ty::DateTime => quote!(time::OffsetDateTime),
        Ty::Json => quote!(serde_json::Value),
        Ty::JsonObject => quote!(serde_json::Map<String, serde_json::Value>),
        Ty::Gid(family) => {
            let name = format_ident!("{family}Gid");
            quote!(crate::ids::#name)
        }
        Ty::UserRef => quote!(crate::ids::UserRef),
        Ty::Named(name) if local.contains(name) => {
            let name = ident(name);
            quote!(#name)
        }
        Ty::Named(name) => {
            let name = ident(name);
            quote!(crate::models::#name)
        }
        Ty::Vec(inner) => {
            let inner = ty_tokens(inner, local);
            quote!(Vec<#inner>)
        }
        Ty::Map(key, value) => {
            let key = ty_tokens(key, local);
            let value = ty_tokens(value, local);
            quote!(std::collections::BTreeMap<#key, #value>)
        }
    }
}

/// Whether a scalar needs a serde helper, and so cannot sit inside a `Vec` or a map.
fn needs_helper(ty: &Ty) -> bool {
    matches!(ty, Ty::Date | Ty::DateTime | Ty::Decimal)
}

/// Renders items, recording the serde helpers they use.
pub struct Renderer<'a> {
    /// Families that have an `opt_fields` enum.
    pub field_families: &'a BTreeSet<String>,
    /// Models defined beside the code that uses them, written without a path.
    pub local: &'a BTreeSet<String>,
    /// The serde helpers the rendered items refer to.
    pub used: BTreeSet<Helper>,
}

impl Renderer<'_> {
    /// The tokens of one item.
    pub fn item(&mut self, item: &Item) -> GenResult<TokenStream> {
        match &item.kind {
            ItemKind::Struct { direction, fields } => self.structure(item, *direction, fields),
            ItemKind::Enum { values } => enumeration(item, values),
            ItemKind::Union {
                discriminator,
                variants,
            } => {
                let union = union(item, discriminator, variants);
                let has_fields = self.has_fields(&item.name);
                Ok(quote!(#union #has_fields))
            }
            ItemKind::WriteUnion { variants } => Ok(write_union(item, variants, self.local)),
        }
    }

    /// The `HasFields` impl of a read type the endpoints half made an `opt_fields` enum for.
    fn has_fields(&self, name: &str) -> Option<TokenStream> {
        self.field_families.contains(name).then(|| {
            let ty = ident(name);
            let field_enum = format_ident!("{}", crate::naming::field_enum_name(name));
            quote! {
                impl crate::fields::HasFields for #ty {
                    type Field = crate::fields::#field_enum;
                }
            }
        })
    }

    fn structure(
        &mut self,
        item: &Item,
        direction: Direction,
        fields: &[Field],
    ) -> GenResult<TokenStream> {
        let name = ident(&item.name);
        let docs = doc(&item.doc);
        let fields = fields
            .iter()
            .map(|field| self.field(&item.name, direction, field))
            .collect::<GenResult<Vec<_>>>()?;
        let (derives, has_fields) = match direction {
            Direction::Read => (
                quote!(#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]),
                self.has_fields(&item.name),
            ),
            Direction::Write => (
                quote!(#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, bon::Builder)]),
                None,
            ),
        };
        Ok(quote! {
            #docs
            #derives
            #[non_exhaustive]
            pub struct #name {
                #(#fields)*
            }
            #has_fields
        })
    }

    fn field(
        &mut self,
        owner: &str,
        direction: Direction,
        field: &Field,
    ) -> GenResult<TokenStream> {
        if let Ty::Vec(inner) | Ty::Map(_, inner) = &field.ty
            && needs_helper(inner)
        {
            return Err(format!(
                "`{owner}.{}` holds dates, times or decimals in a list or map, which needs a helper",
                field.json
            ));
        }
        let docs = doc(&field.doc);
        let name = ident(&field.rust);
        let mut serde_args = Vec::new();
        if field.rust.trim_start_matches("r#") != field.json {
            let json = &field.json;
            serde_args.push(quote!(rename = #json));
        }
        let helper = Helper::for_field(&field.ty, direction, field.clearable);
        if let Some(helper) = helper {
            self.used.insert(helper);
            let path = helper.path();
            if direction == Direction::Read {
                serde_args.push(quote!(default));
            }
            serde_args.push(quote!(with = #path));
        }
        if direction == Direction::Write {
            serde_args.push(quote!(skip_serializing_if = "Option::is_none"));
        }
        let serde = (!serde_args.is_empty()).then(|| quote!(#[serde(#(#serde_args),*)]));
        let mut ty = ty_tokens(&field.ty, self.local);
        if field.boxed {
            ty = quote!(Box<#ty>);
        }
        if field.clearable {
            ty = quote!(crate::models::Clearable<#ty>);
        }
        let builder = (direction == Direction::Write
            && (field.clearable || matches!(field.ty, Ty::String | Ty::UserRef)))
        .then(|| quote!(#[builder(into)]));
        Ok(quote! {
            #docs
            #serde
            #builder
            pub #name: Option<#ty>,
        })
    }
}

fn enumeration(item: &Item, values: &[String]) -> GenResult<TokenStream> {
    let name = ident(&item.name);
    let docs = doc(&item.doc);
    let mut variants = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values {
        let variant = variant_name(value);
        if !seen.insert(variant.clone()) {
            return Err(format!(
                "`{}` has two values that make the variant `{variant}`",
                item.name
            ));
        }
        variants.push((format_ident!("{variant}"), value.as_str()));
    }
    let declarations = variants.iter().map(|(variant, value)| {
        let docs = doc(&format!("`{value}`"));
        quote!(#docs #variant,)
    });
    let as_str = variants
        .iter()
        .map(|(variant, value)| quote!(Self::#variant => #value,));
    let from = variants
        .iter()
        .map(|(variant, value)| quote!(#value => Self::#variant,));
    let long = (values.len() >= LONG_ENUM).then(
        || quote!(#[expect(clippy::too_many_lines, reason = "one arm per value Asana lists")]),
    );
    Ok(quote! {
        #docs
        #[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(from = "String", into = "String")]
        #[non_exhaustive]
        pub enum #name {
            #(#declarations)*
            /// A value this crate does not know yet, kept exactly as Asana sent it.
            Unknown(String),
        }

        impl #name {
            /// The value as Asana writes it.
            #long
            pub fn as_str(&self) -> &str {
                match self {
                    #(#as_str)*
                    Self::Unknown(value) => value,
                }
            }
        }

        impl From<String> for #name {
            #long
            fn from(value: String) -> Self {
                match value.as_str() {
                    #(#from)*
                    _ => Self::Unknown(value),
                }
            }
        }

        impl From<#name> for String {
            fn from(value: #name) -> Self {
                match value {
                    #name::Unknown(value) => value,
                    known => known.as_str().to_owned(),
                }
            }
        }

        impl std::fmt::Display for #name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    })
}

fn union(item: &Item, discriminator: &str, variants: &[UnionVariant]) -> TokenStream {
    let name = ident(&item.name);
    let docs = doc(&format!(
        "{} Told apart by `{}`; a kind this crate does not know is kept as `Other`.",
        item.doc,
        discriminator.trim_start_matches('/').replace('/', ".")
    ));
    let declarations = variants.iter().map(|variant| {
        let variant_ident = ident(&variant.name);
        let family = ident(&variant.family);
        let docs = doc(&format!(
            "A [`{}`](crate::models::{}), tagged `{}`.",
            variant.family, variant.family, variant.tag
        ));
        quote!(#docs #variant_ident(Box<crate::models::#family>),)
    });
    let arms = variants.iter().map(|variant| {
        let variant_ident = ident(&variant.name);
        let tag = &variant.tag;
        quote! {
            Some(#tag) => serde_json::from_value(value)
                .map(|inner| Self::#variant_ident(Box::new(inner)))
                .map_err(serde::de::Error::custom),
        }
    });
    quote! {
        #docs
        #[derive(Clone, Debug, PartialEq)]
        #[non_exhaustive]
        pub enum #name {
            #(#declarations)*
            /// A kind this crate does not know yet, kept exactly as Asana sent it.
            Other(serde_json::Value),
        }

        impl<'de> serde::Deserialize<'de> for #name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
                let tag = value
                    .pointer(#discriminator)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                match tag.as_deref() {
                    #(#arms)*
                    _ => Ok(Self::Other(value)),
                }
            }
        }
    }
}

fn write_union(item: &Item, variants: &[(String, Ty)], local: &BTreeSet<String>) -> TokenStream {
    let name = ident(&item.name);
    let docs = doc(&item.doc);
    let declarations = variants.iter().map(|(variant, ty)| {
        let variant = ident(variant);
        let mut tokens = ty_tokens(ty, local);
        if matches!(ty, Ty::Named(_)) {
            tokens = quote!(Box<#tokens>);
        }
        let docs = doc(&match ty {
            Ty::Gid(_) => "An existing one, by GID.".to_owned(),
            Ty::Named(inner) => format!("A new one, described by `{inner}`."),
            _ => "As text.".to_owned(),
        });
        quote!(#docs #variant(#tokens),)
    });
    quote! {
        #docs
        #[derive(Clone, Debug, PartialEq, serde::Serialize)]
        #[serde(untagged)]
        #[non_exhaustive]
        pub enum #name {
            #(#declarations)*
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_valid_rust() {
        assert_eq!(field_name("type"), "r#type");
        assert_eq!(field_name("assigned_by.any"), "assigned_by_any");
        assert_eq!(field_name("X-Hook-Secret"), "x_hook_secret");
        assert_eq!(field_name("self"), "self_");
        assert_eq!(variant_name("dark-pink"), "DarkPink");
        assert_eq!(variant_name("nonBillable"), "NonBillable");
        assert_eq!(variant_name("COMMAND_TICKET"), "CommandTicket");
        assert_eq!(variant_name("unknown"), "UnknownValue");
        assert_eq!(variant_name("1x"), "V1x");
    }
}
