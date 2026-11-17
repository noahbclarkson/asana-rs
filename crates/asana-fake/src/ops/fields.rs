//! Custom fields, their enum options, and the settings that put a field on a project.

use asana_openapi::endpoints::{
    AddCustomFieldSettingForProject, CreateCustomField, CreateEnumOptionForCustomField,
    GetCustomField, GetCustomFieldSettingsForProject, GetCustomFieldsForWorkspace,
    InsertEnumOptionForCustomField, RemoveCustomFieldSettingForProject, UpdateEnumOption,
};
use serde_json::Value;

use crate::answer::{Answer, Call, Refusal, Reply};
use crate::changes::Change;
use crate::model::{Position, insert};
use crate::paging::page;
use crate::router::{Route, serve};
use crate::state::{CustomField, EnumOption, FieldKind, Id, Kind, Setting, State, lookup};

pub(crate) fn routes() -> Vec<Route> {
    vec![
        serve::<GetCustomField>(get),
        serve::<GetCustomFieldsForWorkspace>(for_workspace),
        serve::<CreateCustomField>(create),
        serve::<CreateEnumOptionForCustomField>(create_option),
        serve::<InsertEnumOptionForCustomField>(reorder_option),
        serve::<UpdateEnumOption>(update_option),
        serve::<GetCustomFieldSettingsForProject>(settings),
        serve::<AddCustomFieldSettingForProject>(add_setting_call),
        serve::<RemoveCustomFieldSettingForProject>(remove_setting),
    ]
}

/// Adds a custom field to `workspace`, with enum options named `options` in order.
pub(crate) fn add_field(
    state: &mut State,
    workspace: &Id,
    name: String,
    kind: FieldKind,
    options: &[String],
) -> Result<Id, Refusal> {
    lookup(&state.workspaces, "workspace", workspace)?;
    let gid = state.new_gid(Kind::CustomField);
    state.fields.insert(
        gid.clone(),
        CustomField {
            name,
            kind,
            workspace: workspace.clone(),
            description: String::new(),
            precision: 0,
            options: Vec::new(),
        },
    );
    for option in options {
        add_option(state, &gid, option.clone(), None, &Position::End)?;
    }
    state.emit(Change::added(&gid, workspace));
    Ok(gid)
}

/// Adds an enum option to `field`.
pub(crate) fn add_option(
    state: &mut State,
    field: &Id,
    name: String,
    color: Option<String>,
    position: &Position,
) -> Result<Id, Refusal> {
    let kind = lookup(&state.fields, "custom_field", field)?.kind;
    if !matches!(kind, FieldKind::Enum | FieldKind::MultiEnum) {
        return Err(Refusal::bad_request(
            "custom_field: Only enum and multi-enum fields have options",
        ));
    }
    let gid = state.new_gid(Kind::EnumOption);
    if let Some(record) = state.fields.get_mut(field) {
        insert(&mut record.options, gid.clone(), position, "insert_before")?;
    }
    state.options.insert(
        gid.clone(),
        EnumOption {
            field: field.clone(),
            name,
            color: color.unwrap_or_else(|| "none".to_owned()),
            enabled: true,
        },
    );
    Ok(gid)
}

/// Puts `field` on `project`.
pub(crate) fn add_setting(
    state: &mut State,
    project: &Id,
    field: &Id,
    important: bool,
    position: &Position,
) -> Result<Id, Refusal> {
    let record = lookup(&state.projects, "project", project)?;
    let definition = lookup(&state.fields, "custom_field", field)?;
    if definition.workspace != record.workspace {
        return Err(Refusal::bad_request(format!(
            "custom_field: Custom field {field} is not in the project's workspace"
        )));
    }
    let already = record.settings.iter().any(|setting| {
        state
            .settings
            .get(setting)
            .is_some_and(|s| &s.field == field)
    });
    if already {
        return Err(Refusal::bad_request(format!(
            "custom_field: Custom field with ID {field} is already on the project"
        )));
    }
    // Positions name custom fields; the project keeps settings, so translate.
    let position = match position {
        Position::Before(anchor) | Position::After(anchor) => {
            let setting = record
                .settings
                .iter()
                .find(|setting| {
                    state
                        .settings
                        .get(*setting)
                        .is_some_and(|s| &s.field == anchor)
                })
                .cloned()
                .unwrap_or_else(|| anchor.clone());
            if matches!(position, Position::Before(_)) {
                Position::Before(setting)
            } else {
                Position::After(setting)
            }
        }
        other => other.clone(),
    };
    let gid = state.new_gid(Kind::CustomFieldSetting);
    if let Some(record) = state.projects.get_mut(project) {
        insert(
            &mut record.settings,
            gid.clone(),
            &position,
            "insert_before",
        )?;
    }
    state.settings.insert(
        gid.clone(),
        Setting {
            project: project.clone(),
            field: field.clone(),
            important,
        },
    );
    Ok(gid)
}

fn get(state: &mut State, call: &Call) -> Reply {
    lookup(&state.fields, "custom_field", call.param(0))?;
    Ok(Answer::data(call.selection.one(state, call.param(0))))
}

fn for_workspace(state: &mut State, call: &Call) -> Reply {
    let workspace = call.param(0);
    lookup(&state.workspaces, "workspace", workspace)?;
    let fields: Vec<Id> = state
        .fields
        .iter()
        .filter(|(_, field)| field.workspace == workspace)
        .map(|(gid, _)| gid.clone())
        .collect();
    page(state, call, &fields)
}

fn create(state: &mut State, call: &Call) -> Reply {
    let workspace = call.required("workspace")?;
    let name = call.required("name")?;
    let subtype = call.required("resource_subtype")?;
    let kind = FieldKind::parse(&subtype).ok_or_else(|| {
        Refusal::bad_request(format!(
            "resource_subtype: Unknown custom field type: {subtype}"
        ))
    })?;
    let mut options = Vec::new();
    if let Some(Value::Array(items)) = call.data().get("enum_options") {
        for item in items {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| Refusal::bad_request("enum_options: Each needs a name"))?;
            options.push(name.to_owned());
        }
    }
    let gid = add_field(state, &workspace, name, kind, &options)?;
    if let Some(record) = state.fields.get_mut(&gid) {
        record.description = call.text("description")?.unwrap_or_default();
        if let Some(precision) = call.data().get("precision").and_then(Value::as_u64) {
            record.precision = u32::try_from(precision).unwrap_or(0);
        }
    }
    Ok(Answer::created(call.selection.one(state, &gid)))
}

fn create_option(state: &mut State, call: &Call) -> Reply {
    let field = call.param(0).to_owned();
    let name = call.required("name")?;
    let color = call.text("color")?;
    let position = Position::read(call, Position::End)?;
    let gid = add_option(state, &field, name, color, &position)?;
    if call.flag("enabled")? == Some(false)
        && let Some(option) = state.options.get_mut(&gid)
    {
        option.enabled = false;
    }
    Ok(Answer::created(call.selection.one(state, &gid)))
}

fn reorder_option(state: &mut State, call: &Call) -> Reply {
    let field = call.param(0).to_owned();
    lookup(&state.fields, "custom_field", &field)?;
    let option = call.required("enum_option")?;
    if lookup(&state.options, "enum_option", &option)?.field != field {
        return Err(Refusal::bad_request(format!(
            "enum_option: {option} is not an option of custom field {field}"
        )));
    }
    let position = match (
        call.text("before_enum_option")?,
        call.text("after_enum_option")?,
    ) {
        (Some(before), None) => Position::Before(before),
        (None, Some(after)) => Position::After(after),
        _ => {
            return Err(Refusal::bad_request(
                "before_enum_option, after_enum_option: Exactly one must be given",
            ));
        }
    };
    if let Some(record) = state.fields.get_mut(&field) {
        let old = record.options.clone();
        record.options.retain(|other| other != &option);
        if let Err(refusal) = insert(
            &mut record.options,
            option.clone(),
            &position,
            "before_enum_option",
        ) {
            record.options = old;
            return Err(refusal);
        }
    }
    Ok(Answer::data(call.selection.one(state, &option)))
}

fn update_option(state: &mut State, call: &Call) -> Reply {
    let gid = call.param(0).to_owned();
    lookup(&state.options, "enum_option", &gid)?;
    let name = call.text("name")?;
    let color = call.text("color")?;
    let enabled = call.flag("enabled")?;
    if let Some(option) = state.options.get_mut(&gid) {
        if let Some(name) = name {
            option.name = name;
        }
        if let Some(color) = color {
            option.color = color;
        }
        if let Some(enabled) = enabled {
            option.enabled = enabled;
        }
    }
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn settings(state: &mut State, call: &Call) -> Reply {
    let settings = lookup(&state.projects, "project", call.param(0))?
        .settings
        .clone();
    // Asana lists settings in full, not compact.
    let items = settings
        .iter()
        .map(|gid| call.selection.one(state, gid))
        .collect();
    crate::paging::page_values(call, items)
}

fn add_setting_call(state: &mut State, call: &Call) -> Reply {
    let project = call.param(0).to_owned();
    let field = call.required("custom_field")?;
    let important = call.flag("is_important")?.unwrap_or(false);
    let position = Position::read(call, Position::End)?;
    let gid = add_setting(state, &project, &field, important, &position)?;
    Ok(Answer::data(call.selection.one(state, &gid)))
}

fn remove_setting(state: &mut State, call: &Call) -> Reply {
    let project = call.param(0).to_owned();
    let field = call.required("custom_field")?;
    let record = lookup(&state.projects, "project", &project)?;
    let setting = record
        .settings
        .iter()
        .find(|setting| {
            state
                .settings
                .get(*setting)
                .is_some_and(|s| s.field == field)
        })
        .cloned()
        .ok_or_else(|| {
            Refusal::bad_request(format!(
                "custom_field: Custom field with ID {field} is not on the project"
            ))
        })?;
    if let Some(record) = state.projects.get_mut(&project) {
        record.settings.retain(|other| other != &setting);
    }
    state.settings.remove(&setting);
    Ok(Answer::empty())
}
