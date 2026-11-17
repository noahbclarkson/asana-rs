//! A custom field's value, typed by the field's kind, for reading and for writing.
//!
//! Asana returns a custom field's value in a member named after its kind (`text_value`,
//! `number_value`, `enum_value`, `multi_enum_values`, `date_value`, `people_value`,
//! `reference_value`), and takes it back on a task or project request as one JSON value per field
//! GID in the request's `custom_fields` map: a string, a number, an enum option's GID, an array of
//! GIDs, a `{"date": ...}` or `{"date_time": ...}` object, or `null` to clear it.
//! [`CustomFieldValue`] is that value as one enum, read with [`TaskCustomFields`] and written with
//! [`CustomFieldValue::to_request_json`] or [`custom_fields_map`].
//!
//! ```
//! use asana_client::CustomFieldValue;
//! use asana_client::openapi::ids::{CustomFieldGid, EnumOptionGid};
//! use rust_decimal::Decimal;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let stage: CustomFieldGid = "1201234567890001".parse()?;
//! let approved: EnumOptionGid = "1201234567890002".parse()?;
//! let fee: CustomFieldGid = "1201234567890003".parse()?;
//!
//! let map = asana_client::custom_fields_map([
//!     (stage, CustomFieldValue::enum_option(approved)),
//!     (fee, CustomFieldValue::number(Decimal::new(125_050, 2))),
//! ]);
//! assert_eq!(
//!     serde_json::to_string(&map)?,
//!     r#"{"1201234567890001":"1201234567890002","1201234567890003":1250.5}"#
//! );
//! # Ok(())
//! # }
//! ```

mod read;

use std::collections::BTreeMap;

use asana_openapi::ids::{CustomFieldGid, EnumOptionGid, Gid, UserGid};
use rust_decimal::Decimal;
use serde_json::{Map, Value};
use time::{Date, OffsetDateTime, UtcOffset};

pub use read::{TaskCustomField, TaskCustomFields};

/// The value of one custom field, typed by the field's kind (its `resource_subtype`).
///
/// [`Empty`](Self::Empty) is the one way to say "no value": a field that is not set when read,
/// and `null`, which clears the field, when written.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CustomFieldValue {
    /// No value: the field is not set (read), or is to be cleared (written).
    Empty,
    /// A `text` field's value.
    Text(String),
    /// A `number` field's value, as a decimal rather than a floating-point number. Asana sends
    /// and stores numbers as doubles, so it is read through the shortest text of the double Asana
    /// sent (`1250.5`, never `1250.4999999999999`); see [`asana_openapi::number`].
    Number(Decimal),
    /// An `enum` field's chosen option.
    Enum(EnumOptionRef),
    /// A `multi_enum` field's chosen options, in Asana's order.
    MultiEnum(Vec<EnumOptionRef>),
    /// A `date` field's date, and its time when one was chosen.
    Date(CustomDate),
    /// A `people` field's users.
    People(Vec<PersonRef>),
    /// A `reference` field's referenced objects.
    Reference(Vec<ResourceRef>),
    /// A kind this crate does not know yet, or a value of a known kind it could not read.
    ///
    /// When read, `raw` is an object holding the value members this crate's model of a custom
    /// field knows (`display_value`, `text_value`, `number_value` and the rest), as Asana sent
    /// them; a member the model does not know is not kept. When written, `raw` is sent exactly as
    /// it is, so [`CustomFieldValue::raw`] can send a value this crate cannot type.
    Unknown {
        /// The field's `resource_subtype` as Asana wrote it; empty when it was absent or when the
        /// value was made with [`CustomFieldValue::raw`].
        resource_subtype: String,
        /// The value, as described above.
        raw: Value,
    },
}

/// An enum option, as a custom field's value names it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EnumOptionRef {
    /// The option's GID: all a request needs.
    pub gid: EnumOptionGid,
    /// The option's name, when read.
    pub name: Option<String>,
    /// The option's colour, when read.
    pub color: Option<String>,
    /// Whether the option can still be chosen, when read.
    pub enabled: Option<bool>,
}

impl EnumOptionRef {
    /// An option named by its GID alone, as a request needs it.
    pub fn new(gid: EnumOptionGid) -> Self {
        Self {
            gid,
            name: None,
            color: None,
            enabled: None,
        }
    }
}

impl From<EnumOptionGid> for EnumOptionRef {
    fn from(gid: EnumOptionGid) -> Self {
        Self::new(gid)
    }
}

/// A user, as a `people` field's value names one.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PersonRef {
    /// The user's GID: all a request needs.
    pub gid: UserGid,
    /// The user's name, when read.
    pub name: Option<String>,
    /// The user's email address, when read and selected.
    pub email: Option<String>,
}

impl PersonRef {
    /// A user named by GID alone, as a request needs it.
    pub fn new(gid: UserGid) -> Self {
        Self {
            gid,
            name: None,
            email: None,
        }
    }
}

impl From<UserGid> for PersonRef {
    fn from(gid: UserGid) -> Self {
        Self::new(gid)
    }
}

/// An object a `reference` field points at, of any kind.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResourceRef {
    /// The object's GID, as Asana wrote it.
    pub gid: String,
    /// The object's `resource_type`, for example `task`, when read.
    pub resource_type: Option<String>,
    /// The object's name, when read.
    pub name: Option<String>,
}

impl ResourceRef {
    /// An object named by a typed GID, as a request needs it.
    pub fn new(gid: &impl Gid) -> Self {
        Self {
            gid: gid.as_str().to_owned(),
            resource_type: None,
            name: None,
        }
    }
}

/// A `date` field's value: a day, and the moment when the field holds a time as well.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CustomDate {
    /// The day, as Asana's `date` (`YYYY-MM-DD`).
    pub date: Date,
    /// The moment, as Asana's `date_time`, when a time was chosen.
    pub date_time: Option<OffsetDateTime>,
}

impl CustomDate {
    /// A day with no time.
    pub fn on(date: Date) -> Self {
        Self {
            date,
            date_time: None,
        }
    }

    /// A moment. Its day is the day in UTC, as Asana keeps it.
    pub fn at(date_time: OffsetDateTime) -> Self {
        let date = date_time
            .checked_to_offset(UtcOffset::UTC)
            .unwrap_or(date_time)
            .date();
        Self {
            date,
            date_time: Some(date_time),
        }
    }
}

impl CustomFieldValue {
    /// A `text` field's value.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// A `number` field's value. Written as a JSON number: an integer when it is one, otherwise
    /// the nearest double, which carries about 15 significant digits (Asana stores numbers as
    /// doubles). [`asana_openapi::number::fits_a_double`] says whether a value survives whole.
    pub fn number(number: impl Into<Decimal>) -> Self {
        Self::Number(number.into())
    }

    /// An `enum` field's option, by its GID (or an [`EnumOptionRef`] read earlier).
    pub fn enum_option(option: impl Into<EnumOptionRef>) -> Self {
        Self::Enum(option.into())
    }

    /// A `multi_enum` field's options, by their GIDs. An empty list clears the field.
    pub fn multi_enum<O: Into<EnumOptionRef>>(options: impl IntoIterator<Item = O>) -> Self {
        Self::MultiEnum(options.into_iter().map(Into::into).collect())
    }

    /// A `date` field's day, with no time.
    pub fn date(date: Date) -> Self {
        Self::Date(CustomDate::on(date))
    }

    /// A `date` field's day and time. Written in UTC, to the millisecond.
    pub fn date_time(date_time: OffsetDateTime) -> Self {
        Self::Date(CustomDate::at(date_time))
    }

    /// A `people` field's users, by their GIDs. An empty list clears the field.
    pub fn people<P: Into<PersonRef>>(people: impl IntoIterator<Item = P>) -> Self {
        Self::People(people.into_iter().map(Into::into).collect())
    }

    /// A `reference` field's objects. An empty list clears the field.
    pub fn reference(objects: impl IntoIterator<Item = ResourceRef>) -> Self {
        Self::Reference(objects.into_iter().collect())
    }

    /// Clears the field: `null` in the request.
    pub fn clear() -> Self {
        Self::Empty
    }

    /// A value this crate has no variant for, sent exactly as given.
    pub fn raw(value: Value) -> Self {
        Self::Unknown {
            resource_subtype: String::new(),
            raw: value,
        }
    }

    /// Whether the field holds nothing.
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// The text, for a `text` field.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The number, for a `number` field.
    pub fn as_number(&self) -> Option<Decimal> {
        match self {
            Self::Number(number) => Some(*number),
            _ => None,
        }
    }

    /// The chosen option, for an `enum` field.
    pub fn as_enum_option(&self) -> Option<&EnumOptionRef> {
        match self {
            Self::Enum(option) => Some(option),
            _ => None,
        }
    }

    /// The chosen options, for a `multi_enum` field.
    pub fn as_multi_enum(&self) -> Option<&[EnumOptionRef]> {
        match self {
            Self::MultiEnum(options) => Some(options),
            _ => None,
        }
    }

    /// The date, for a `date` field.
    pub fn as_date(&self) -> Option<&CustomDate> {
        match self {
            Self::Date(date) => Some(date),
            _ => None,
        }
    }

    /// The users, for a `people` field.
    pub fn as_people(&self) -> Option<&[PersonRef]> {
        match self {
            Self::People(people) => Some(people),
            _ => None,
        }
    }

    /// The JSON Asana's `custom_fields` request map takes for this value:
    ///
    /// | Value | JSON |
    /// |---|---|
    /// | [`Empty`](Self::Empty) | `null` |
    /// | [`Text`](Self::Text) | `"text"` |
    /// | [`Number`](Self::Number) | a JSON number: `1250.50` is written `1250.5`, see below |
    /// | [`Enum`](Self::Enum) | the option's GID, `"1201234567890002"` |
    /// | [`MultiEnum`](Self::MultiEnum), [`People`](Self::People), [`Reference`](Self::Reference) | an array of GIDs |
    /// | [`Date`](Self::Date) without a time | `{"date": "2026-12-17"}` |
    /// | [`Date`](Self::Date) with a time | `{"date_time": "2026-12-17T02:30:00.000Z"}`, in UTC |
    /// | [`Unknown`](Self::Unknown) | `raw`, as it is |
    ///
    /// A number that is an integer fitting `i64` or `u64` is written as that integer; any other
    /// is written as the nearest double, which prints the decimal's own digits whenever a double
    /// can carry them (about 15 significant digits) and is still sent, rounded, when it cannot.
    /// Trailing zeros are not part of a JSON number. This never fails.
    pub fn to_request_json(&self) -> Value {
        match self {
            Self::Empty => Value::Null,
            Self::Text(text) => Value::String(text.clone()),
            Self::Number(number) => number_json(*number),
            Self::Enum(option) => Value::String(option.gid.as_str().to_owned()),
            Self::MultiEnum(options) => gids(options.iter().map(|option| option.gid.as_str())),
            Self::Date(date) => date_json(date),
            Self::People(people) => gids(people.iter().map(|person| person.gid.as_str())),
            Self::Reference(objects) => gids(objects.iter().map(|object| object.gid.as_str())),
            Self::Unknown { raw, .. } => raw.clone(),
        }
    }
}

/// Builds the `custom_fields` map of a task or project request from `(field, value)` pairs, each
/// value written by [`CustomFieldValue::to_request_json`]. A later pair for the same field
/// replaces an earlier one.
pub fn custom_fields_map(
    pairs: impl IntoIterator<Item = (CustomFieldGid, CustomFieldValue)>,
) -> BTreeMap<CustomFieldGid, Value> {
    pairs
        .into_iter()
        .map(|(field, value)| (field, value.to_request_json()))
        .collect()
}

/// A decimal as a JSON number, as [`asana_openapi::number::to_json_number`] writes it.
fn number_json(number: Decimal) -> Value {
    Value::Number(asana_openapi::number::to_json_number(number))
}

fn gids<'a>(gids: impl Iterator<Item = &'a str>) -> Value {
    Value::Array(gids.map(|gid| Value::String(gid.to_owned())).collect())
}

fn date_json(date: &CustomDate) -> Value {
    let mut object = Map::new();
    match date.date_time {
        Some(moment) => object.insert("date_time".to_owned(), Value::String(utc_millis(moment))),
        None => object.insert("date".to_owned(), Value::String(day(date.date))),
    };
    Value::Object(object)
}

/// `YYYY-MM-DD`.
fn day(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, the form Asana's examples use. A moment that cannot be moved to UTC
/// (only at the very ends of the representable range) keeps its own offset.
fn utc_millis(moment: OffsetDateTime) -> String {
    let utc = moment.checked_to_offset(UtcOffset::UTC).unwrap_or(moment);
    let offset = utc.offset();
    let zone = if offset.is_utc() {
        "Z".to_owned()
    } else {
        let (hours, minutes, _) = offset.as_hms();
        let sign = if offset.is_negative() { '-' } else { '+' };
        format!(
            "{sign}{:02}:{:02}",
            hours.unsigned_abs(),
            minutes.unsigned_abs()
        )
    };
    format!(
        "{}T{:02}:{:02}:{:02}.{:03}{zone}",
        day(utc.date()),
        utc.hour(),
        utc.minute(),
        utc.second(),
        utc.millisecond()
    )
}
