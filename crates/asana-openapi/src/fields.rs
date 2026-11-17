//! Choosing which fields Asana returns (`opt_fields`).
//!
//! By default Asana returns each item of a list as a compact record (little more than `gid` and
//! `name`), and a single object (one read, created or updated) as its full record. A request names
//! the fields it wants as dotted paths, such as `memberships.section.name`; naming them widens a
//! list's items and narrows a single object to what was named. The
//! specification lists the paths each resource accepts, so each resource has a generated enum of
//! them and the [`fields!`](crate::fields!) macro turns a misspelt path into a compile error.

use std::borrow::Cow;

pub use crate::generated::fields::*;

/// One `opt_fields` path of some resource. Implemented by the generated, fieldless enums.
pub trait FieldPath: Copy + Send + Sync + 'static {
    /// The path as Asana expects it, for example `memberships.section.name`.
    fn path(self) -> &'static str;
}

/// A resource whose fields can be chosen. Implemented by the generated models.
pub trait HasFields {
    /// The enum of this resource's `opt_fields` paths.
    type Field: FieldPath;
}

/// A selection of fields of one resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fields<F> {
    listed: Vec<F>,
    raw: Vec<Cow<'static, str>>,
}

impl<F> Default for Fields<F> {
    fn default() -> Self {
        Self {
            listed: Vec::new(),
            raw: Vec::new(),
        }
    }
}

impl<F: FieldPath> Fields<F> {
    /// An empty selection: Asana returns its default record (compact for each item of a list,
    /// full for a single object).
    pub fn none() -> Self {
        Self::default()
    }

    /// Adds a field.
    #[must_use]
    pub fn with(mut self, field: F) -> Self {
        self.listed.push(field);
        self
    }

    /// Adds a path the specification does not list for this resource, sent to Asana as written.
    /// For a field Asana has added since this crate was generated.
    #[must_use]
    pub fn with_raw(mut self, path: impl Into<Cow<'static, str>>) -> Self {
        self.raw.push(path.into());
        self
    }

    /// Whether nothing was chosen.
    pub fn is_empty(&self) -> bool {
        self.listed.is_empty() && self.raw.is_empty()
    }

    /// The chosen paths: the listed fields in the order they were added, then the raw ones.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.listed
            .iter()
            .map(|field| field.path())
            .chain(self.raw.iter().map(AsRef::as_ref))
    }

    /// The `opt_fields` query value: the paths joined by commas, or `None` when nothing was chosen.
    pub fn to_query_value(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        Some(self.paths().collect::<Vec<_>>().join(","))
    }
}

impl<F: FieldPath> FromIterator<F> for Fields<F> {
    fn from_iter<I: IntoIterator<Item = F>>(iter: I) -> Self {
        Self {
            listed: iter.into_iter().collect(),
            raw: Vec::new(),
        }
    }
}

impl<F: FieldPath, const N: usize> From<[F; N]> for Fields<F> {
    fn from(fields: [F; N]) -> Self {
        Self {
            listed: fields.into(),
            raw: Vec::new(),
        }
    }
}

/// Compares two strings in a `const fn`. Used by the generated `lookup` functions.
#[doc(hidden)]
pub const fn const_str_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// Chooses fields of a resource, checked when the code is compiled.
///
/// ```
/// use asana_openapi::fields;
/// use asana_openapi::fields::TaskField;
///
/// let chosen = fields![Task: name, due_on, memberships.section.name];
/// assert_eq!(chosen.to_query_value().as_deref(), Some("name,due_on,memberships.section.name"));
/// let same: asana_openapi::fields::Fields<TaskField> = chosen;
/// ```
///
/// A path the specification does not list for that resource does not compile:
///
/// ```compile_fail
/// let chosen = asana_openapi::fields![Task: nmae];
/// ```
#[macro_export]
macro_rules! fields {
    ($resource:ident : $($first:ident $(. $rest:ident)*),+ $(,)?) => {
        $crate::fields::Fields::from([
            $(
                const {
                    match <$crate::models::$resource as $crate::fields::HasFields>::Field::lookup(
                        concat!(stringify!($first) $(, ".", stringify!($rest))*)
                    ) {
                        ::std::option::Option::Some(field) => field,
                        ::std::option::Option::None => panic!(concat!(
                            "`", stringify!($first) $(, ".", stringify!($rest))*,
                            "` is not an opt_fields path of `", stringify!($resource), "`"
                        )),
                    }
                }
            ),+
        ])
    };
}
