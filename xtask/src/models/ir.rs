//! The generator's picture of the models before they become Rust: types, fields and items.

use super::Direction;

/// The Rust type of a field, an array element or a map value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    /// `bool`.
    Bool,
    /// `i64`: Asana's `integer`, used for counts and sizes.
    Integer,
    /// `rust_decimal::Decimal`: Asana's `number`, used for money, rates and custom field numbers,
    /// which must not round.
    Decimal,
    /// `String`.
    String,
    /// `time::Date`, written `YYYY-MM-DD`.
    Date,
    /// `time::OffsetDateTime`, written in RFC 3339.
    DateTime,
    /// `serde_json::Value`: anything.
    Json,
    /// `serde_json::Map<String, serde_json::Value>`: an object with some known and any other keys.
    JsonObject,
    /// The typed GID of a family: `crate::ids::{family}Gid`.
    Gid(String),
    /// `crate::ids::UserRef`: `me`, an email address or a user's GID, in a request.
    UserRef,
    /// A generated model: `crate::models::{name}`.
    Named(String),
    /// `Vec<T>`.
    Vec(Box<Ty>),
    /// `BTreeMap<K, V>`.
    Map(Box<Ty>, Box<Ty>),
}

impl Ty {
    /// The generated model this type is, without looking inside a `Vec` or a map.
    pub fn named(&self) -> Option<&str> {
        match self {
            Self::Named(name) => Some(name),
            _ => None,
        }
    }
}

/// One field of a generated struct.
#[derive(Clone, Debug)]
pub struct Field {
    /// The JSON key.
    pub json: String,
    /// The Rust field name (a raw identifier where the key is a keyword).
    pub rust: String,
    /// Its type, without the `Option` every field has.
    pub ty: Ty,
    /// Its doc comment.
    pub doc: String,
    /// In a request, whether `null` may be sent to clear it.
    pub clearable: bool,
    /// Whether the type is boxed, because it contains the struct that holds it.
    pub boxed: bool,
}

/// A variant of a union read from a response.
#[derive(Clone, Debug)]
pub struct UnionVariant {
    /// The variant name.
    pub name: String,
    /// The family it holds.
    pub family: String,
    /// The discriminator value that selects it.
    pub tag: String,
}

/// What an item is.
#[derive(Clone, Debug)]
pub enum ItemKind {
    /// A struct read from responses or written in requests.
    Struct {
        /// Which way it goes.
        direction: Direction,
        /// Its fields, in the specification's order.
        fields: Vec<Field>,
    },
    /// A string enum that keeps values it does not know.
    Enum {
        /// The values the specification lists.
        values: Vec<String>,
    },
    /// A union read from a response, told apart by the value at `discriminator`.
    Union {
        /// The JSON pointer of the discriminating string, such as `/resource_type`.
        discriminator: String,
        /// The variants.
        variants: Vec<UnionVariant>,
    },
    /// A union written in a request, serialised as whichever variant it holds.
    WriteUnion {
        /// The variants: a name and the type it holds.
        variants: Vec<(String, Ty)>,
    },
}

/// One generated item.
#[derive(Clone, Debug)]
pub struct Item {
    /// Its Rust name.
    pub name: String,
    /// Its doc comment.
    pub doc: String,
    /// The family whose file it goes in, or `None` for the shared file.
    pub home: Option<String>,
    /// What it is.
    pub kind: ItemKind,
}
