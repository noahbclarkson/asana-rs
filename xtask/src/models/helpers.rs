//! The serde helpers the generated models need, and the `Clearable` type for nullable request
//! fields. Only the helpers some field uses are emitted, so none is dead code.

use std::collections::BTreeSet;

use proc_macro2::TokenStream;
use quote::quote;

use super::Direction;
use super::ir::Ty;

/// A serde `with` module a field can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Helper {
    /// `Option<time::Date>` read from a response.
    DateRead,
    /// `Option<time::Date>` written in a request.
    DateWrite,
    /// `Option<time::OffsetDateTime>`, either way: `time`'s own RFC 3339 module.
    DateTime,
    /// `Option<rust_decimal::Decimal>`, either way: `rust_decimal`'s exact-number module.
    Decimal,
    /// `Option<Clearable<time::Date>>` in a request.
    ClearableDate,
    /// `Option<Clearable<time::OffsetDateTime>>` in a request.
    ClearableDateTime,
    /// `Option<Clearable<rust_decimal::Decimal>>` in a request.
    ClearableDecimal,
}

impl Helper {
    /// The helper a field of this type needs, if any.
    pub fn for_field(ty: &Ty, direction: Direction, clearable: bool) -> Option<Self> {
        Some(match (ty, direction, clearable) {
            (Ty::Date, Direction::Read, _) => Self::DateRead,
            (Ty::Date, Direction::Write, false) => Self::DateWrite,
            (Ty::Date, Direction::Write, true) => Self::ClearableDate,
            (Ty::DateTime, Direction::Write, true) => Self::ClearableDateTime,
            (Ty::DateTime, _, _) => Self::DateTime,
            (Ty::Decimal, Direction::Write, true) => Self::ClearableDecimal,
            (Ty::Decimal, _, _) => Self::Decimal,
            _ => return None,
        })
    }

    /// The module path, as a string for `#[serde(with = ...)]`.
    pub fn path(self) -> &'static str {
        match self {
            Self::DateRead | Self::DateWrite => "crate::models::serde_helpers::date_option",
            Self::DateTime => "time::serde::rfc3339::option",
            Self::Decimal => "rust_decimal::serde::arbitrary_precision_option",
            Self::ClearableDate => "crate::models::serde_helpers::clearable_date",
            Self::ClearableDateTime => "crate::models::serde_helpers::clearable_date_time",
            Self::ClearableDecimal => "crate::models::serde_helpers::clearable_decimal",
        }
    }
}

/// The `serde_helpers` module, holding only what `used` names; `None` when nothing needs it.
pub fn module(used: &BTreeSet<Helper>) -> Option<TokenStream> {
    let date_read = used.contains(&Helper::DateRead).then(|| {
        quote! {
            pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Option<Date>, D::Error> {
                let Some(text) = Option::<String>::deserialize(deserializer)? else {
                    return Ok(None);
                };
                Date::parse(&text, FORMAT).map(Some).map_err(serde::de::Error::custom)
            }
        }
    });
    let date_write = used.contains(&Helper::DateWrite).then(|| {
        quote! {
            pub(crate) fn serialize<S: Serializer>(
                value: &Option<Date>,
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                match value {
                    Some(date) => serializer.serialize_str(&super::format(*date)?),
                    None => serializer.serialize_none(),
                }
            }
        }
    });
    let date_module = (date_read.is_some() || date_write.is_some()).then(|| {
        let uses = match (date_read.is_some(), date_write.is_some()) {
            (true, true) => quote!(
                use serde::{Deserialize, Deserializer, Serializer};
            ),
            (true, false) => quote!(
                use serde::{Deserialize, Deserializer};
            ),
            _ => quote!(
                use serde::Serializer;
            ),
        };
        let import_format = date_read.is_some().then(|| {
            quote!(
                use super::FORMAT;
            )
        });
        quote! {
            /// `Option<time::Date>` as Asana's `YYYY-MM-DD`.
            pub(crate) mod date_option {
                #uses
                use time::Date;

                #import_format
                #date_read
                #date_write
            }
        }
    });
    let clearable = |helper: Helper, module: &str, ty: TokenStream, write: TokenStream| {
        used.contains(&helper).then(|| {
            let module = quote::format_ident!("{module}");
            quote! {
                /// A nullable request field: absent, `null`, or a value.
                pub(crate) mod #module {
                    use serde::Serializer;

                    use crate::models::Clearable;

                    pub(crate) fn serialize<S: Serializer>(
                        value: &Option<Clearable<#ty>>,
                        serializer: S,
                    ) -> Result<S::Ok, S::Error> {
                        match value {
                            Some(Clearable::Set(inner)) => #write,
                            Some(Clearable::Clear) | None => serializer.serialize_none(),
                        }
                    }
                }
            }
        })
    };
    let clearable_date = clearable(
        Helper::ClearableDate,
        "clearable_date",
        quote!(time::Date),
        quote!(serializer.serialize_str(&super::format(*inner)?)),
    );
    let clearable_date_time = clearable(
        Helper::ClearableDateTime,
        "clearable_date_time",
        quote!(time::OffsetDateTime),
        quote!(time::serde::rfc3339::serialize(inner, serializer)),
    );
    let clearable_decimal = clearable(
        Helper::ClearableDecimal,
        "clearable_decimal",
        quote!(rust_decimal::Decimal),
        quote!(rust_decimal::serde::arbitrary_precision::serialize(
            inner, serializer
        )),
    );
    let needs_format = used.iter().any(|helper| {
        matches!(
            helper,
            Helper::DateRead | Helper::DateWrite | Helper::ClearableDate
        )
    });
    let needs_writer = used.contains(&Helper::DateWrite) || used.contains(&Helper::ClearableDate);
    if !needs_format && clearable_date_time.is_none() && clearable_decimal.is_none() {
        return None;
    }
    let format_constant = needs_format.then(|| {
        quote! {
            /// Asana's date format.
            const FORMAT: &[time::format_description::BorrowedFormatItem<'static>] =
                time::macros::format_description!("[year]-[month]-[day]");
        }
    });
    let writer = needs_writer.then(|| {
        quote! {
            /// Writes a date in [`FORMAT`], turning a formatting failure into the serializer's error.
            fn format<E: serde::ser::Error>(date: time::Date) -> Result<String, E> {
                date.format(FORMAT).map_err(E::custom)
            }
        }
    });
    Some(quote! {
        #![doc = " Serde helpers for the scalars Asana writes its own way: dates as `YYYY-MM-DD`, and nullable request fields."]

        #format_constant
        #writer
        #date_module
        #clearable_date
        #clearable_date_time
        #clearable_decimal
    })
}

/// The `Clearable` type and its serialisation.
pub fn clearable_module() -> TokenStream {
    quote! {
        #![doc = " A request field that can be cleared."]

        /// A value for a request field that Asana lets a caller clear by sending `null`, such as a
        /// task's due date.
        ///
        /// The field itself is an `Option`: `None` leaves the field out of the request, so Asana
        /// keeps what it has. `Some(Clearable::Set(value))` sets it and `Some(Clearable::Clear)`
        /// sends `null`. A two-variant enum reads better at the call site than
        /// `Option<Option<T>>`: with the builders, `.due_on(date)` sets the field, through
        /// `From<T>`, and `.due_on(Clearable::Clear)` clears it, where `Option<Option<T>>` would
        /// make `.due_on(None)` mean "send null", which is easy to misread as "leave alone".
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub enum Clearable<T> {
            /// Set the field to this value.
            Set(T),
            /// Send `null`, clearing the field.
            Clear,
        }

        impl<T> Clearable<T> {
            /// The value being set, or `None` when the field is being cleared.
            pub fn value(&self) -> Option<&T> {
                match self {
                    Self::Set(value) => Some(value),
                    Self::Clear => None,
                }
            }
        }

        impl<T> From<T> for Clearable<T> {
            fn from(value: T) -> Self {
                Self::Set(value)
            }
        }

        impl<T: serde::Serialize> serde::Serialize for Clearable<T> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    Self::Set(value) => value.serialize(serializer),
                    Self::Clear => serializer.serialize_none(),
                }
            }
        }
    }
}
