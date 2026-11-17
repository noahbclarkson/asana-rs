//! Custom field definitions and their enum options.
//!
//! `client.custom_fields()` reads a field's definition and a workspace's fields, finds an enum
//! option by name, and creates, renames or disables options. Reading and writing a field's value
//! on a task is [`CustomFieldValue`](crate::CustomFieldValue)'s.

use asana_openapi::endpoints::{
    AddCustomFieldSettingForProject, CreateEnumOptionForCustomField, GetCustomField,
    GetCustomFieldsForWorkspace, RemoveCustomFieldSettingForProject, UpdateEnumOption,
};
use asana_openapi::fields;
use asana_openapi::fields::{CustomFieldField, CustomFieldSettingField, EnumOptionField};
use asana_openapi::ids::{
    CustomFieldGid, CustomFieldSettingGid, EnumOptionGid, Gid, ProjectGid, WorkspaceGid,
};
use asana_openapi::models::{
    AddCustomFieldSettingRequest, AddCustomFieldSettingRequestCustomField, EnumOption,
    EnumOptionInput, EnumOptionRequest, RemoveCustomFieldSettingRequest,
};

use super::request::{Request, opt_fields_setter};
use crate::{Client, Error};

impl Client {
    /// Custom field definitions and their enum options.
    pub fn custom_fields(&self) -> CustomFields<'_> {
        CustomFields { client: self }
    }
}

/// The custom fields handle: `client.custom_fields()`.
#[derive(Clone, Copy, Debug)]
pub struct CustomFields<'a> {
    client: &'a Client,
}

/// What a lookup by name found. Asana does not keep names unique, so a name can match several
/// objects; this says so instead of choosing silently.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum NameMatch<T> {
    /// Nothing has that name.
    Missing,
    /// Exactly one object has that name.
    One(T),
    /// Several objects have that name, in Asana's order.
    Several(Vec<T>),
}

impl<T> NameMatch<T> {
    /// Sorts a list of matches into the three cases.
    pub fn from_matches(mut matches: Vec<T>) -> Self {
        match matches.len() {
            0 => Self::Missing,
            1 => matches.pop().map_or(Self::Missing, Self::One),
            _ => Self::Several(matches),
        }
    }

    /// The match when there is exactly one.
    pub fn one(self) -> Option<T> {
        match self {
            Self::One(found) => Some(found),
            Self::Missing | Self::Several(_) => None,
        }
    }

    /// The first match in Asana's order, however many there are.
    pub fn first(self) -> Option<T> {
        match self {
            Self::Missing => None,
            Self::One(found) => Some(found),
            Self::Several(found) => found.into_iter().next(),
        }
    }

    /// Whether several objects share the name.
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Several(_))
    }
}

opt_fields_setter!(
    GetCustomField => CustomFieldField,
    GetCustomFieldsForWorkspace => CustomFieldField,
    CreateEnumOptionForCustomField => EnumOptionField,
    UpdateEnumOption => EnumOptionField,
    AddCustomFieldSettingForProject => CustomFieldSettingField,
);

impl<'a> CustomFields<'a> {
    /// A custom field's full definition, its enum options included.
    ///
    /// `GET /custom_fields/{custom_field_gid}` (`getCustomField`); scope `custom_fields:read`;
    /// retried on a server error, as every read is.
    pub fn get(&self, field: CustomFieldGid) -> Request<'a, GetCustomField> {
        Request::new(self.client, GetCustomField::builder(field).build())
    }

    /// The custom fields of a workspace, as a list to stream or page.
    ///
    /// `GET /workspaces/{workspace_gid}/custom_fields` (`getCustomFieldsForWorkspace`); scope
    /// `custom_fields:read`; retried on a server error.
    pub fn list(&self, workspace: WorkspaceGid) -> Request<'a, GetCustomFieldsForWorkspace> {
        Request::new(
            self.client,
            GetCustomFieldsForWorkspace::builder(workspace).build(),
        )
    }

    /// The field's enabled enum options named `name`: compared exactly, case included, after
    /// trimming spaces at both ends (as section lookups do).
    ///
    /// Reads the field's options with one `getCustomField` (scope `custom_fields:read`, retried
    /// on a server error).
    ///
    /// # Errors
    ///
    /// As [`Client::send`].
    pub async fn option_by_name(
        &self,
        field: &CustomFieldGid,
        name: &str,
    ) -> Result<NameMatch<EnumOption>, Error> {
        self.options_named(field, name, false).await
    }

    /// As [`option_by_name`](Self::option_by_name), but disabled options match too.
    ///
    /// # Errors
    ///
    /// As [`Client::send`].
    pub async fn option_by_name_including_disabled(
        &self,
        field: &CustomFieldGid,
        name: &str,
    ) -> Result<NameMatch<EnumOption>, Error> {
        self.options_named(field, name, true).await
    }

    /// The enabled option named `name`, made if the field has none.
    ///
    /// Reads the options (`getCustomField`); when none is named `name`, creates one
    /// (`createEnumOptionForCustomField`, scope `custom_fields:write`, never retried because it
    /// creates). When several share the name, the first in Asana's order is returned. A disabled
    /// option of that name is left as it is.
    ///
    /// Two callers may race to make the same option. When the creation fails for any reason
    /// (Asana refusing a duplicate, or a timeout that leaves the outcome unknown), the options are
    /// read again, and an option of that name found then is returned instead of the error. Asana
    /// may also accept both creations, which leaves two options of that name; later calls return
    /// the first.
    ///
    /// # Errors
    ///
    /// The error of the first read; or the creation's error when the second read finds no
    /// option of that name either.
    pub async fn ensure_option(
        &self,
        field: &CustomFieldGid,
        name: &str,
    ) -> Result<EnumOption, Error> {
        if let Some(found) = self.option_by_name(field, name).await?.first() {
            return Ok(found);
        }
        match self.create_option(field.clone(), name).send().await {
            Ok(created) => Ok(created),
            Err(error) => match self.option_by_name(field, name).await {
                Ok(found) => found.first().ok_or(error),
                Err(_) => Err(error),
            },
        }
    }

    /// Adds an enum option named `name` to the field, last unless placed with
    /// [`insert_before`](Request::insert_before) or [`insert_after`](Request::insert_after).
    ///
    /// `POST /custom_fields/{custom_field_gid}/enum_options` (`createEnumOptionForCustomField`);
    /// scope `custom_fields:write`; never retried on a server error, because it creates.
    pub fn create_option(
        &self,
        field: CustomFieldGid,
        name: impl Into<String>,
    ) -> Request<'a, CreateEnumOptionForCustomField> {
        Request::new(
            self.client,
            CreateEnumOptionForCustomField::builder(field)
                .data(EnumOptionRequest::builder().name(name).build())
                .build(),
        )
    }

    /// Changes an enum option; set what changes with [`name`](Request::name),
    /// [`enabled`](Request::enabled) and [`color`](Request::color).
    ///
    /// `PUT /enum_options/{enum_option_gid}` (`updateEnumOption`); scope `custom_fields:write`;
    /// retried on a server error, because a `PUT` is safe to repeat.
    pub fn update_option(&self, option: EnumOptionGid) -> Request<'a, UpdateEnumOption> {
        Request::new(
            self.client,
            UpdateEnumOption::builder(option)
                .data(EnumOptionInput::default())
                .build(),
        )
    }

    /// Renames an enum option: [`update_option`](Self::update_option) with a new name.
    pub fn rename_option(
        &self,
        option: EnumOptionGid,
        name: impl Into<String>,
    ) -> Request<'a, UpdateEnumOption> {
        self.update_option(option).name(name)
    }

    /// Disables an enum option, so it can no longer be chosen; tasks that hold it keep it. Asana
    /// has no way to delete an option.
    pub fn disable_option(&self, option: EnumOptionGid) -> Request<'a, UpdateEnumOption> {
        self.update_option(option).enabled(false)
    }

    /// Adds an existing custom field to a project, last among its fields unless placed with
    /// [`insert_before`](Request::insert_before) or [`insert_after`](Request::insert_after). A
    /// project's settings are read with `client.projects().custom_field_settings(project)`.
    ///
    /// `POST /projects/{project_gid}/addCustomFieldSetting` (`addCustomFieldSettingForProject`);
    /// scope `projects:write`; never retried on a server error, because Asana does not mark it
    /// safe to repeat.
    pub fn add_to_project(
        &self,
        project: ProjectGid,
        field: CustomFieldGid,
    ) -> Request<'a, AddCustomFieldSettingForProject> {
        let data = AddCustomFieldSettingRequest::builder()
            .custom_field(AddCustomFieldSettingRequestCustomField::Gid(field))
            .build();
        Request::new(
            self.client,
            AddCustomFieldSettingForProject::builder(project)
                .data(data)
                .build(),
        )
    }

    /// Removes a custom field from a project; the field's definition is kept.
    ///
    /// `POST /projects/{project_gid}/removeCustomFieldSetting`
    /// (`removeCustomFieldSettingForProject`); scope `projects:write`; never retried on a server
    /// error.
    pub fn remove_from_project(
        &self,
        project: ProjectGid,
        field: CustomFieldGid,
    ) -> Request<'a, RemoveCustomFieldSettingForProject> {
        let data = RemoveCustomFieldSettingRequest::builder()
            .custom_field(field)
            .build();
        Request::new(
            self.client,
            RemoveCustomFieldSettingForProject::builder(project)
                .data(data)
                .build(),
        )
    }

    async fn options_named(
        &self,
        field: &CustomFieldGid,
        name: &str,
        include_disabled: bool,
    ) -> Result<NameMatch<EnumOption>, Error> {
        let definition = self
            .get(field.clone())
            .fields(fields![CustomField: name, enum_options.name, enum_options.enabled, enum_options.color])
            .send()
            .await?;
        let matches = definition
            .enum_options
            .unwrap_or_default()
            .into_iter()
            .filter(|option| {
                option
                    .name
                    .as_deref()
                    .is_some_and(|own| own.trim() == name.trim())
            })
            .filter(|option| include_disabled || option.enabled != Some(false))
            .collect();
        Ok(NameMatch::from_matches(matches))
    }
}

impl Request<'_, CreateEnumOptionForCustomField> {
    /// The option's colour, for example `blue`.
    pub fn color(self, color: impl Into<String>) -> Self {
        self.map(|endpoint| {
            endpoint.data.get_or_insert_with(Default::default).color = Some(color.into());
        })
    }

    /// Places the option just before an existing one.
    pub fn insert_before(self, option: &EnumOptionGid) -> Self {
        let option = option.as_str().to_owned();
        self.map(|endpoint| {
            endpoint
                .data
                .get_or_insert_with(Default::default)
                .insert_before = Some(option);
        })
    }

    /// Places the option just after an existing one.
    pub fn insert_after(self, option: &EnumOptionGid) -> Self {
        let option = option.as_str().to_owned();
        self.map(|endpoint| {
            endpoint
                .data
                .get_or_insert_with(Default::default)
                .insert_after = Some(option);
        })
    }
}

impl Request<'_, UpdateEnumOption> {
    /// The option's new name.
    pub fn name(self, name: impl Into<String>) -> Self {
        self.map(|endpoint| {
            endpoint.data.get_or_insert_with(Default::default).name = Some(name.into());
        })
    }

    /// Whether the option can be chosen.
    pub fn enabled(self, enabled: bool) -> Self {
        self.map(|endpoint| {
            endpoint.data.get_or_insert_with(Default::default).enabled = Some(enabled);
        })
    }

    /// The option's colour, for example `blue`.
    pub fn color(self, color: impl Into<String>) -> Self {
        self.map(|endpoint| {
            endpoint.data.get_or_insert_with(Default::default).color = Some(color.into());
        })
    }
}

impl Request<'_, AddCustomFieldSettingForProject> {
    /// Whether the field is shown prominently in the project, for example as a list column.
    pub fn important(self, important: bool) -> Self {
        self.map(|endpoint| endpoint.data.is_important = Some(important))
    }

    /// Places the field just before one of the project's existing custom field settings.
    pub fn insert_before(self, setting: &CustomFieldSettingGid) -> Self {
        let setting = setting.as_str().to_owned();
        self.map(|endpoint| endpoint.data.insert_before = Some(setting))
    }

    /// Places the field just after one of the project's existing custom field settings.
    pub fn insert_after(self, setting: &CustomFieldSettingGid) -> Self {
        let setting = setting.as_str().to_owned();
        self.map(|endpoint| endpoint.data.insert_after = Some(setting))
    }
}
