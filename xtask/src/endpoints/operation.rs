//! Reads the specification's operations into [`Operation`]s: one plain record per operation that
//! the rest of the endpoints half renders without looking at the specification again.

use heck::{ToSnakeCase, ToUpperCamelCase};
use openapiv3::{
    Parameter, ParameterSchemaOrContent, PathItem, ReferenceOr, RequestBody, Response, Schema,
    SchemaKind, StatusCode, StringFormat, Type, VariantOrUnknownOrEmpty,
};

use crate::GenResult;
use crate::naming;
use crate::spec::Spec;

/// The HTTP method of an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    /// The variant of `asana_openapi::endpoint::Method` it becomes.
    pub fn variant(self) -> &'static str {
        match self {
            Self::Get => "Get",
            Self::Post => "Post",
            Self::Put => "Put",
            Self::Delete => "Delete",
        }
    }

    /// As written on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

/// One operation of the specification, ready to render.
pub struct Operation<'a> {
    /// The `operationId`, for example `getTask`.
    pub id: String,
    /// The descriptor's type name, for example `GetTask`.
    pub type_name: String,
    pub method: Method,
    /// The path template, verbatim.
    pub route: String,
    /// The operation's first tag, which decides its module.
    pub tag: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub deprecated: bool,
    /// Path parameters in the order they appear in the route.
    pub path_params: Vec<Param>,
    /// Query parameters in specification order, `opt_fields` included.
    pub query_params: Vec<Param>,
    pub body: Body<'a>,
    pub response: ResponseShape,
    /// The granular OAuth scope, when the operation names one.
    pub scope: Option<String>,
}

/// One path or query parameter.
pub struct Param {
    /// The name in the specification, for example `created_on.before`.
    pub name: String,
    /// The Rust field name, for example `created_on_before` (or `r#type`).
    pub field: String,
    pub kind: ParamKind,
    pub required: bool,
    pub description: Option<String>,
}

/// What a parameter's value is in Rust.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamKind {
    /// A typed GID, by its type name (`TaskGid`).
    Gid(String),
    /// `ids::UserRef`: `me`, an email address or a GID.
    UserRef,
    /// Free text.
    Text,
    Bool,
    /// A non-negative count.
    Integer,
    /// The page size (`limit`), as `PageLimit`.
    Limit,
    /// The paging position (`offset`), as `Offset`.
    Offset,
    /// A `YYYY-MM-DD` date. `nullable` adds the value `null` ("has no date").
    Date {
        nullable: bool,
    },
    /// An RFC 3339 timestamp.
    DateTime,
    /// A closed set of values, generated as an enum named `type_name`.
    Enum {
        type_name: String,
        values: Vec<String>,
    },
    /// An array sent comma-joined (`style: form, explode: false`).
    List,
    /// `opt_fields`, as `Fields<{type}Field>` of the resource `family`.
    OptFields {
        family: String,
        values: Vec<String>,
    },
}

/// What an operation sends.
pub enum Body<'a> {
    None,
    /// `{"data": <schema>}`.
    Json {
        data: Box<ReferenceOr<Schema>>,
        required: bool,
        description: Option<&'a str>,
    },
    /// A multipart form described by this object schema.
    Multipart(&'a Schema),
}

/// What an operation's success response holds.
pub enum ResponseShape {
    /// `{"data": <schema>}`, with `next_page` beside it for a list.
    Data(Data),
    /// The whole body is the result, because it carries siblings of `data`.
    Whole {
        /// The siblings of `data`, in specification order.
        siblings: Vec<String>,
        data: Data,
    },
}

/// The schema of `data`.
pub enum Data {
    /// One object.
    One(ReferenceOr<Schema>),
    /// An array of objects.
    Many(ReferenceOr<Schema>),
}

impl Data {
    /// The schema of one element: the object itself or the array's item.
    pub fn element(&self) -> &ReferenceOr<Schema> {
        match self {
            Self::One(schema) | Self::Many(schema) => schema,
        }
    }
}

impl Operation<'_> {
    /// Whether the descriptor is a paged list: it takes `offset` and returns an array.
    pub fn paginated(&self) -> bool {
        let returns_array = matches!(self.response, ResponseShape::Data(Data::Many(_)));
        returns_array
            && self
                .query_params
                .iter()
                .any(|param| param.kind == ParamKind::Offset)
    }

    /// The resource family whose fields `opt_fields` chooses, with the paths the operation lists.
    pub fn opt_fields(&self) -> Option<(&str, &[String])> {
        self.query_params
            .iter()
            .find_map(|param| match &param.kind {
                ParamKind::OptFields { family, values } => {
                    Some((family.as_str(), values.as_slice()))
                }
                _ => None,
            })
    }
}

/// Query parameters a client never wants: `opt_pretty` only indents the JSON Asana sends back.
const SKIPPED_QUERY: &[&str] = &["opt_pretty"];

/// Query parameters that always name one kind of object, so they take that object's typed GID.
/// Anything not listed stays text: `parent`, `target`, `resource` and `member` name objects of
/// several kinds, and `*.any`/`*.not`/`*.all` take comma-separated lists.
const QUERY_GIDS: &[(&str, &str)] = &[
    ("organization", "Workspace"),
    ("portfolio", "Portfolio"),
    ("project", "Project"),
    ("section", "Section"),
    ("task", "Task"),
    ("team", "Team"),
    ("workspace", "Workspace"),
];

/// Query parameters that name a user, which Asana accepts as `me`, an email address or a GID.
const QUERY_USERS: &[&str] = &["assignee", "owner", "user"];

/// Path parameters whose stem is not the resource family's name: `{record_gid}` names a
/// `RecordItem` and `{role_gid}` an `RbacRole`.
const PATH_FAMILY_OVERRIDES: &[(&str, &str)] = &[("record", "RecordItem"), ("role", "RbacRole")];

/// Reads every operation, in specification order.
pub fn collect<'a>(spec: &'a Spec, gid_families: &[String]) -> GenResult<Vec<Operation<'a>>> {
    let mut operations = Vec::new();
    for (route, item) in &spec.api.paths.paths {
        let ReferenceOr::Item(item) = item else {
            return Err(format!("{route}: a referenced path item is not supported"));
        };
        for (method, operation) in item.iter() {
            operations.push(
                read(spec, gid_families, route, item, method, operation)
                    .map_err(|error| format!("{method} {route}: {error}"))?,
            );
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for operation in &operations {
        if !seen.insert(operation.type_name.as_str()) {
            return Err(format!("two operations are named {}", operation.type_name));
        }
    }
    Ok(operations)
}

fn read<'a>(
    spec: &'a Spec,
    gid_families: &[String],
    route: &str,
    item: &'a PathItem,
    method: &str,
    operation: &'a openapiv3::Operation,
) -> GenResult<Operation<'a>> {
    let method = match method {
        "get" => Method::Get,
        "post" => Method::Post,
        "put" => Method::Put,
        "delete" => Method::Delete,
        other => return Err(format!("the method {other} is not supported")),
    };
    let id = operation
        .operation_id
        .clone()
        .ok_or("the operation has no operationId")?;
    let type_name = id.to_upper_camel_case();
    let tag = operation
        .tags
        .first()
        .cloned()
        .ok_or("the operation has no tag")?;

    // An operation's parameter replaces the path item's of the same name and location.
    let mut parameters: Vec<&Parameter> = Vec::new();
    for parameter in item.parameters.iter().chain(&operation.parameters) {
        let parameter = resolve_parameter(spec, parameter)?;
        let same = |known: &&Parameter| {
            known.parameter_data_ref().name == parameter.parameter_data_ref().name
                && std::mem::discriminant(*known) == std::mem::discriminant(parameter)
        };
        match parameters.iter().position(same) {
            Some(index) => parameters[index] = parameter,
            None => parameters.push(parameter),
        }
    }

    let mut path_params = Vec::new();
    let mut query_params = Vec::new();
    for parameter in parameters {
        match parameter {
            Parameter::Path { parameter_data, .. } => {
                path_params.push(path_param(parameter_data, gid_families)?);
            }
            Parameter::Query { parameter_data, .. } => {
                if SKIPPED_QUERY.contains(&parameter_data.name.as_str()) {
                    continue;
                }
                query_params.push(query_param(
                    spec,
                    &type_name,
                    parameter_data,
                    gid_families,
                    operation,
                )?);
            }
            Parameter::Header { parameter_data, .. } | Parameter::Cookie { parameter_data, .. } => {
                return Err(format!(
                    "the header or cookie parameter {} is not supported",
                    parameter_data.name
                ));
            }
        }
    }
    order_path_params(route, &mut path_params)?;

    Ok(Operation {
        id,
        type_name,
        method,
        route: route.to_owned(),
        tag,
        summary: operation.summary.clone(),
        description: operation.description.clone(),
        deprecated: operation.deprecated,
        path_params,
        query_params,
        body: body(spec, operation)?,
        response: response(spec, operation)?,
        scope: scope(spec, operation)?,
    })
}

/// Sorts path parameters into the order the route names them, and checks it names each once.
fn order_path_params(route: &str, params: &mut Vec<Param>) -> GenResult<()> {
    let named: Vec<&str> = route
        .split('{')
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
        .collect();
    if named.len() != params.len() {
        return Err(format!(
            "the route names {} parameters and the operation declares {}",
            named.len(),
            params.len()
        ));
    }
    let mut ordered = Vec::with_capacity(params.len());
    for name in named {
        let index = params
            .iter()
            .position(|param| param.name == name)
            .ok_or_else(|| format!("the route's {{{name}}} is not declared"))?;
        ordered.push(params.remove(index));
    }
    *params = ordered;
    Ok(())
}

fn resolve_parameter<'a>(
    spec: &'a Spec,
    parameter: &'a ReferenceOr<Parameter>,
) -> GenResult<&'a Parameter> {
    match parameter {
        ReferenceOr::Item(parameter) => Ok(parameter),
        ReferenceOr::Reference { reference } => {
            let name = reference
                .strip_prefix("#/components/parameters/")
                .ok_or_else(|| format!("{reference} is not a component parameter"))?;
            match spec
                .api
                .components
                .as_ref()
                .and_then(|components| components.parameters.get(name))
            {
                Some(ReferenceOr::Item(parameter)) => Ok(parameter),
                _ => Err(format!("the parameter {reference} is missing or indirect")),
            }
        }
    }
}

/// Resolves a schema reference to the component schema it names.
pub fn resolve_schema<'a>(
    spec: &'a Spec,
    schema: &'a ReferenceOr<Schema>,
) -> GenResult<&'a Schema> {
    match schema {
        ReferenceOr::Item(schema) => Ok(schema),
        ReferenceOr::Reference { reference } => {
            let name = schema_name(reference)?;
            match spec
                .api
                .components
                .as_ref()
                .and_then(|components| components.schemas.get(name))
            {
                Some(ReferenceOr::Item(schema)) => Ok(schema),
                _ => Err(format!("the schema {reference} is missing or indirect")),
            }
        }
    }
}

/// The component name in `#/components/schemas/<name>`.
pub fn schema_name(reference: &str) -> GenResult<&str> {
    reference
        .strip_prefix("#/components/schemas/")
        .ok_or_else(|| format!("{reference} is not a component schema"))
}

fn schema_of(data: &openapiv3::ParameterData) -> GenResult<&ReferenceOr<Schema>> {
    match &data.format {
        ParameterSchemaOrContent::Schema(schema) => Ok(schema),
        ParameterSchemaOrContent::Content(_) => Err(format!(
            "the parameter {} has content, not a schema",
            data.name
        )),
    }
}

fn path_param(data: &openapiv3::ParameterData, gid_families: &[String]) -> GenResult<Param> {
    let kind = if data.name == "user_gid" {
        ParamKind::UserRef
    } else if let Some(stem) = data.name.strip_suffix("_gid") {
        let family = PATH_FAMILY_OVERRIDES
            .iter()
            .find(|(from, _)| *from == stem)
            .map_or_else(|| stem.to_upper_camel_case(), |(_, to)| (*to).to_owned());
        if !gid_families.contains(&family) {
            return Err(format!(
                "the path parameter {} needs a GID type for the family {family}",
                data.name
            ));
        }
        ParamKind::Gid(naming::gid_type_name(&family))
    } else {
        ParamKind::Text
    };
    Ok(Param {
        name: data.name.clone(),
        field: field_name(&data.name),
        kind,
        required: true,
        description: data.description.clone(),
    })
}

fn query_param(
    spec: &Spec,
    type_name: &str,
    data: &openapiv3::ParameterData,
    gid_families: &[String],
    operation: &openapiv3::Operation,
) -> GenResult<Param> {
    let name = data.name.as_str();
    let schema = resolve_schema(spec, schema_of(data)?)?;
    let kind = match name {
        "limit" => ParamKind::Limit,
        "offset" => ParamKind::Offset,
        "opt_fields" => opt_fields(spec, schema, operation)?,
        _ => value_kind(type_name, name, schema, gid_families)?,
    };
    Ok(Param {
        name: name.to_owned(),
        field: field_name(name),
        kind,
        required: data.required,
        description: data.description.clone(),
    })
}

fn value_kind(
    type_name: &str,
    name: &str,
    schema: &Schema,
    gid_families: &[String],
) -> GenResult<ParamKind> {
    let SchemaKind::Type(ty) = &schema.schema_kind else {
        return Err(format!("the query parameter {name} has a composite schema"));
    };
    Ok(match ty {
        Type::Boolean(_) => ParamKind::Bool,
        Type::Integer(_) => ParamKind::Integer,
        Type::Array(_) => ParamKind::List,
        Type::String(string) => {
            let values: Vec<String> = string.enumeration.iter().flatten().cloned().collect();
            if !values.is_empty() {
                ParamKind::Enum {
                    type_name: format!("{type_name}{}", name.to_upper_camel_case()),
                    values,
                }
            } else if let VariantOrUnknownOrEmpty::Item(format) = &string.format {
                match format {
                    StringFormat::Date => ParamKind::Date {
                        nullable: schema.schema_data.nullable,
                    },
                    StringFormat::DateTime => ParamKind::DateTime,
                    _ => ParamKind::Text,
                }
            } else if QUERY_USERS.contains(&name) {
                ParamKind::UserRef
            } else if let Some((_, family)) = QUERY_GIDS.iter().find(|(query, _)| *query == name)
                && gid_families.iter().any(|known| known == family)
            {
                ParamKind::Gid(naming::gid_type_name(family))
            } else {
                ParamKind::Text
            }
        }
        Type::Number(_) | Type::Object(_) => {
            return Err(format!(
                "the query parameter {name} has an unsupported type"
            ));
        }
    })
}

fn opt_fields(
    spec: &Spec,
    schema: &Schema,
    operation: &openapiv3::Operation,
) -> GenResult<ParamKind> {
    let SchemaKind::Type(Type::Array(array)) = &schema.schema_kind else {
        return Err("opt_fields is not an array".to_owned());
    };
    let items = array
        .items
        .as_ref()
        .ok_or("opt_fields has no items")?
        .clone()
        .unbox();
    let items = resolve_schema(spec, &items)?;
    let SchemaKind::Type(Type::String(string)) = &items.schema_kind else {
        return Err("opt_fields items are not strings".to_owned());
    };
    let values: Vec<String> = string.enumeration.iter().flatten().cloned().collect();
    if values.is_empty() {
        return Err("opt_fields lists no paths".to_owned());
    }
    let shape = response(spec, operation)?;
    let element = match &shape {
        ResponseShape::Data(data) | ResponseShape::Whole { data, .. } => data.element(),
    };
    let ReferenceOr::Reference { reference } = element else {
        return Err("opt_fields is offered but data is not a named schema".to_owned());
    };
    let family = naming::role_of(schema_name(reference)?)
        .type_name()
        .to_owned();
    Ok(ParamKind::OptFields { family, values })
}

/// The Rust field name for a parameter: snake case, dots to underscores, keywords raw.
pub fn field_name(name: &str) -> String {
    let snake = name.replace('.', "_").to_snake_case();
    if syn::parse_str::<syn::Ident>(&snake).is_ok() {
        snake
    } else {
        format!("r#{snake}")
    }
}

fn body<'a>(spec: &'a Spec, operation: &'a openapiv3::Operation) -> GenResult<Body<'a>> {
    let Some(body) = &operation.request_body else {
        return Ok(Body::None);
    };
    let body: &RequestBody = match body {
        ReferenceOr::Item(body) => body,
        ReferenceOr::Reference { reference } => {
            return Err(format!("the request body {reference} is a reference"));
        }
    };
    let mut content = body.content.iter();
    let (Some((media_type, media)), None) = (content.next(), content.next()) else {
        return Err("the request body has more or fewer than one media type".to_owned());
    };
    let schema = media
        .schema
        .as_ref()
        .ok_or("the request body has no schema")?;
    match media_type.as_str() {
        "multipart/form-data" => Ok(Body::Multipart(resolve_schema(spec, schema)?)),
        "application/json" => {
            let object = object_properties(resolve_schema(spec, schema)?)?;
            let keys: Vec<&str> = object.properties.keys().map(String::as_str).collect();
            if keys != ["data"] {
                return Err(format!("the request body has the properties {keys:?}"));
            }
            let data = object
                .properties
                .get("data")
                .ok_or("the request body has no data")?
                .clone()
                .unbox();
            Ok(Body::Json {
                data: Box::new(data),
                required: body.required,
                description: body.description.as_deref(),
            })
        }
        other => Err(format!("the request media type {other} is not supported")),
    }
}

fn object_properties(schema: &Schema) -> GenResult<&openapiv3::ObjectType> {
    match &schema.schema_kind {
        SchemaKind::Type(Type::Object(object)) => Ok(object),
        _ => Err("the envelope is not an object".to_owned()),
    }
}

fn response(spec: &Spec, operation: &openapiv3::Operation) -> GenResult<ResponseShape> {
    let (_, success) = operation
        .responses
        .responses
        .iter()
        .find(|(status, _)| matches!(status, StatusCode::Code(200..=299)))
        .ok_or("the operation has no success response")?;
    let success: &Response = match success {
        ReferenceOr::Item(response) => response,
        ReferenceOr::Reference { reference } => {
            let name = reference
                .strip_prefix("#/components/responses/")
                .ok_or_else(|| format!("{reference} is not a component response"))?;
            match spec
                .api
                .components
                .as_ref()
                .and_then(|components| components.responses.get(name))
            {
                Some(ReferenceOr::Item(response)) => response,
                _ => return Err(format!("the response {reference} is missing or indirect")),
            }
        }
    };
    let media = success
        .content
        .get("application/json")
        .ok_or("the success response is not JSON")?;
    let envelope = resolve_schema(
        spec,
        media
            .schema
            .as_ref()
            .ok_or("the success response has no schema")?,
    )?;
    let object = object_properties(envelope)?;
    let data = object
        .properties
        .get("data")
        .ok_or("the success response has no data")?
        .clone()
        .unbox();
    let data = match &data {
        ReferenceOr::Item(Schema {
            schema_kind: SchemaKind::Type(Type::Array(array)),
            ..
        }) => Data::Many(
            array
                .items
                .as_ref()
                .ok_or("data is an array without items")?
                .clone()
                .unbox(),
        ),
        _ => Data::One(data),
    };
    let siblings: Vec<String> = object
        .properties
        .keys()
        .filter(|key| !matches!(key.as_str(), "data" | "next_page"))
        .cloned()
        .collect();
    if siblings.is_empty() {
        Ok(ResponseShape::Data(data))
    } else {
        Ok(ResponseShape::Whole { siblings, data })
    }
}

/// The granular OAuth scope an operation lists. An operation lists at most one; one that lists
/// none works with a personal access token or the `default` scope.
fn scope(spec: &Spec, operation: &openapiv3::Operation) -> GenResult<Option<String>> {
    let requirements = operation
        .security
        .as_ref()
        .or(spec.api.security.as_ref())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let scopes: Vec<&String> = requirements
        .iter()
        .filter_map(|requirement| requirement.get("oauth2"))
        .flatten()
        .collect();
    match scopes.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some((*one).clone())),
        _ => Err(format!(
            "the operation lists several OAuth scopes: {scopes:?}"
        )),
    }
}
