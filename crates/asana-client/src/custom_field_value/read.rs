//! Reading custom field values from a task or a project, by each field's kind.

use asana_openapi::ids::CustomFieldGid;
use asana_openapi::models::{
    AsanaNamedResource, CustomField, CustomFieldResourceSubtype, EnumOption, Project, Task, User,
};
use serde_json::{Map, Value};

use super::{CustomDate, CustomFieldValue, EnumOptionRef, PersonRef, ResourceRef};

/// One custom field on a task or project, with its value typed.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct TaskCustomField {
    /// The field's GID: the same as the custom field definition's.
    pub gid: CustomFieldGid,
    /// The field's name, when selected.
    pub name: Option<String>,
    /// The field's kind, from `resource_subtype` (or the deprecated `type` when only that came).
    pub resource_subtype: Option<CustomFieldResourceSubtype>,
    /// Asana's own text for the value, which suits display and needs no knowledge of the kind.
    pub display_value: Option<String>,
    /// The value. [`CustomFieldValue::Empty`] when the field is not set, and also when the value
    /// members were not selected (select `custom_fields`, which brings them all).
    pub value: CustomFieldValue,
}

impl TaskCustomField {
    /// Reads one entry of a `custom_fields` array. `None` when the entry carries no GID.
    pub fn read(field: &CustomField) -> Option<Self> {
        Some(Self {
            gid: field.gid.clone()?,
            name: field.name.clone(),
            resource_subtype: kind(field),
            display_value: field.display_value.clone(),
            value: CustomFieldValue::from_field(field),
        })
    }
}

/// Typed custom field values on a model that carries a `custom_fields` array.
///
/// Every method answers from what the response holds: when `custom_fields` was not selected in
/// `opt_fields` there are no fields, which is `None` or an empty list, never an error.
///
/// ```
/// use asana_client::{CustomFieldValue, TaskCustomFields};
/// use asana_client::openapi::models::Task;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let task: Task = serde_json::from_str(r#"{
///     "gid": "1201234567890123",
///     "custom_fields": [{
///         "gid": "1201234567890001", "name": "Stage", "resource_subtype": "text",
///         "text_value": "Drafting", "display_value": "Drafting"
///     }]
/// }"#)?;
/// let stage = task.custom_field_named("Stage").map(|field| field.value);
/// assert_eq!(stage, Some(CustomFieldValue::text("Drafting")));
/// # Ok(())
/// # }
/// ```
pub trait TaskCustomFields {
    /// Every custom field present, in Asana's order.
    fn custom_fields_typed(&self) -> Vec<TaskCustomField>;

    /// The field with this GID, if present.
    fn custom_field(&self, gid: &CustomFieldGid) -> Option<TaskCustomField>;

    /// The first field whose name is exactly `name` (case and spacing included), if present.
    /// Names are not unique in Asana; prefer [`custom_field`](Self::custom_field) where the GID is
    /// known.
    fn custom_field_named(&self, name: &str) -> Option<TaskCustomField>;
}

fn typed(fields: Option<&Vec<CustomField>>) -> Vec<TaskCustomField> {
    fields
        .into_iter()
        .flatten()
        .filter_map(TaskCustomField::read)
        .collect()
}

fn by_gid(fields: Option<&Vec<CustomField>>, gid: &CustomFieldGid) -> Option<TaskCustomField> {
    fields?
        .iter()
        .find(|field| field.gid.as_ref() == Some(gid))
        .and_then(TaskCustomField::read)
}

fn by_name(fields: Option<&Vec<CustomField>>, name: &str) -> Option<TaskCustomField> {
    fields?
        .iter()
        .filter(|field| field.name.as_deref() == Some(name))
        .find_map(TaskCustomField::read)
}

macro_rules! custom_fields_on {
    ($($model:ty),+) => { $(
        impl TaskCustomFields for $model {
            fn custom_fields_typed(&self) -> Vec<TaskCustomField> {
                typed(self.custom_fields.as_ref())
            }

            fn custom_field(&self, gid: &CustomFieldGid) -> Option<TaskCustomField> {
                by_gid(self.custom_fields.as_ref(), gid)
            }

            fn custom_field_named(&self, name: &str) -> Option<TaskCustomField> {
                by_name(self.custom_fields.as_ref(), name)
            }
        }
    )+ };
}

custom_fields_on!(Task, Project);

impl CustomFieldValue {
    /// Reads a custom field's value by its kind. A kind this crate does not know, or a value it
    /// cannot read (an option or user without a GID), is [`CustomFieldValue::Unknown`].
    pub fn from_field(field: &CustomField) -> Self {
        let Some(kind) = kind(field) else {
            return unknown(field, String::new());
        };
        let value = match &kind {
            CustomFieldResourceSubtype::Text => {
                Some(field.text_value.clone().map_or(Self::Empty, Self::Text))
            }
            CustomFieldResourceSubtype::Number => {
                Some(field.number_value.map_or(Self::Empty, Self::Number))
            }
            CustomFieldResourceSubtype::Enum => match &field.enum_value {
                None => Some(Self::Empty),
                Some(option) => option_ref(option).map(Self::Enum),
            },
            CustomFieldResourceSubtype::MultiEnum => list(
                field.multi_enum_values.as_ref(),
                option_ref,
                Self::MultiEnum,
            ),
            CustomFieldResourceSubtype::Date => Some(date(field)),
            CustomFieldResourceSubtype::People => {
                list(field.people_value.as_ref(), person_ref, Self::People)
            }
            CustomFieldResourceSubtype::Reference => list(
                field.reference_value.as_ref(),
                resource_ref,
                Self::Reference,
            ),
            _ => None,
        };
        value.unwrap_or_else(|| unknown(field, kind.as_str().to_owned()))
    }
}

/// The field's kind: `resource_subtype`, or the deprecated `type` when only that was sent.
fn kind(field: &CustomField) -> Option<CustomFieldResourceSubtype> {
    field.resource_subtype.clone().or_else(|| {
        field
            .r#type
            .as_ref()
            .map(|kind| CustomFieldResourceSubtype::from(kind.as_str().to_owned()))
    })
}

/// A list value: `Empty` when absent or empty, `None` (so `Unknown`) when an item is unreadable.
fn list<T, R>(
    items: Option<&Vec<T>>,
    read: impl Fn(&T) -> Option<R>,
    wrap: impl FnOnce(Vec<R>) -> CustomFieldValue,
) -> Option<CustomFieldValue> {
    match items {
        None => Some(CustomFieldValue::Empty),
        Some(items) if items.is_empty() => Some(CustomFieldValue::Empty),
        Some(items) => items.iter().map(read).collect::<Option<Vec<_>>>().map(wrap),
    }
}

fn date(field: &CustomField) -> CustomFieldValue {
    let Some(value) = &field.date_value else {
        return CustomFieldValue::Empty;
    };
    match (value.date, value.date_time) {
        (Some(date), date_time) => CustomFieldValue::Date(CustomDate { date, date_time }),
        (None, Some(date_time)) => CustomFieldValue::Date(CustomDate::at(date_time)),
        (None, None) => CustomFieldValue::Empty,
    }
}

fn option_ref(option: &EnumOption) -> Option<EnumOptionRef> {
    Some(EnumOptionRef {
        gid: option.gid.clone()?,
        name: option.name.clone(),
        color: option.color.clone(),
        enabled: option.enabled,
    })
}

fn person_ref(user: &User) -> Option<PersonRef> {
    Some(PersonRef {
        gid: user.gid.clone()?,
        name: user.name.clone(),
        email: user.email.clone(),
    })
}

fn resource_ref(object: &AsanaNamedResource) -> Option<ResourceRef> {
    Some(ResourceRef {
        gid: object.gid.clone()?,
        resource_type: object.resource_type.clone(),
        name: object.name.clone(),
    })
}

fn unknown(field: &CustomField, resource_subtype: String) -> CustomFieldValue {
    CustomFieldValue::Unknown {
        resource_subtype,
        raw: raw(field),
    }
}

/// The value members of `field` this crate's model knows, as JSON, leaving out the absent ones.
fn raw(field: &CustomField) -> Value {
    let mut object = Map::new();
    let mut put = |name: &str, value: Option<Value>| {
        if let Some(value) = value {
            object.insert(name.to_owned(), value);
        }
    };
    put(
        "display_value",
        field.display_value.clone().map(Value::String),
    );
    put("text_value", field.text_value.clone().map(Value::String));
    put("number_value", field.number_value.map(super::number_json));
    put("enum_value", field.enum_value.as_deref().map(option_json));
    put(
        "multi_enum_values",
        field
            .multi_enum_values
            .as_ref()
            .map(|options| Value::Array(options.iter().map(option_json).collect())),
    );
    put(
        "date_value",
        field.date_value.as_ref().map(|value| {
            let mut date = Map::new();
            date.insert(
                "date".to_owned(),
                value
                    .date
                    .map_or(Value::Null, |day| Value::String(super::day(day))),
            );
            date.insert(
                "date_time".to_owned(),
                value.date_time.map_or(Value::Null, |moment| {
                    Value::String(super::utc_millis(moment))
                }),
            );
            Value::Object(date)
        }),
    );
    put(
        "people_value",
        field.people_value.as_ref().map(|people| {
            Value::Array(
                people
                    .iter()
                    .map(|user| {
                        named(
                            user.gid.as_ref().map(|gid| gid.to_string()),
                            None,
                            user.name.as_ref(),
                        )
                    })
                    .collect(),
            )
        }),
    );
    put(
        "reference_value",
        field.reference_value.as_ref().map(|objects| {
            Value::Array(
                objects
                    .iter()
                    .map(|object| {
                        named(
                            object.gid.clone(),
                            object.resource_type.as_ref(),
                            object.name.as_ref(),
                        )
                    })
                    .collect(),
            )
        }),
    );
    Value::Object(object)
}

fn option_json(option: &EnumOption) -> Value {
    let mut object = Map::new();
    if let Some(gid) = &option.gid {
        object.insert("gid".to_owned(), Value::String(gid.to_string()));
    }
    if let Some(name) = &option.name {
        object.insert("name".to_owned(), Value::String(name.clone()));
    }
    if let Some(color) = &option.color {
        object.insert("color".to_owned(), Value::String(color.clone()));
    }
    if let Some(enabled) = option.enabled {
        object.insert("enabled".to_owned(), Value::Bool(enabled));
    }
    Value::Object(object)
}

fn named(gid: Option<String>, resource_type: Option<&String>, name: Option<&String>) -> Value {
    let mut object = Map::new();
    if let Some(gid) = gid {
        object.insert("gid".to_owned(), Value::String(gid));
    }
    if let Some(resource_type) = resource_type {
        object.insert(
            "resource_type".to_owned(),
            Value::String(resource_type.clone()),
        );
    }
    if let Some(name) = name {
        object.insert("name".to_owned(), Value::String(name.clone()));
    }
    Value::Object(object)
}
