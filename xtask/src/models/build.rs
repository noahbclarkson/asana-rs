//! Turns schemas into the generator's items: one struct per read family, request structs, string
//! enums, unions and the inline objects they hold.
//!
//! Every component schema is named the same way whichever caller asks, so the endpoints half and
//! the models half agree: a read family is its family name; a request schema is its own name; a
//! read schema used in a request is its `…Input` projection (its writable properties only); any
//! other schema is its own name. Inline objects are named after their owner and field.

use std::collections::{BTreeMap, BTreeSet};

use heck::ToUpperCamelCase;
use serde_json::Value;

use super::Direction;
use super::classify::Classes;
use super::docs;
use super::ir::{Field, Item, ItemKind, Ty, UnionVariant};
use super::schema;
use crate::GenResult;
use crate::naming::{SchemaRole, role_of};

/// The pointers tried, in order, to tell the members of a read union apart.
const DISCRIMINATORS: &[&str] = &[
    "/resource_type",
    "/resource_subtype",
    "/resource/resource_type",
];

/// Where a schema is being turned into a type.
#[derive(Clone)]
struct Ctx<'s> {
    direction: Direction,
    /// The type whose field this is; inline types are named after it.
    owner: String,
    /// The JSON key of the field.
    field: String,
    /// The file the owner goes in.
    home: Option<String>,
    /// The field's description, for the parts of a field (array items) that have none.
    description: Option<&'s str>,
}

/// Builds items, remembering what each component schema became.
pub struct Builder<'a> {
    raw: &'a Value,
    classes: &'a Classes,
    /// Items an earlier build made, which this one may refer to but not change.
    base: Option<&'a BTreeMap<String, Item>>,
    /// What a component schema became, in each direction.
    base_memo: Option<&'a BTreeMap<(String, Direction), Ty>>,
    /// Items made by this build.
    pub items: BTreeMap<String, Item>,
    /// What a component schema became, in each direction, in this build.
    pub memo: BTreeMap<(String, Direction), Ty>,
    /// The type names component schemas give, so an inline type never takes one.
    reserved: BTreeSet<String>,
    /// The type names of schemas that are neither a resource family nor a request.
    others: BTreeSet<String>,
}

impl<'a> Builder<'a> {
    /// A builder for the whole specification.
    pub fn new(raw: &'a Value, classes: &'a Classes) -> GenResult<Self> {
        let mut reserved = BTreeSet::new();
        let mut others = BTreeSet::new();
        for (name, _) in schema::components(raw)? {
            let role = role_of(name);
            reserved.insert(role.type_name().to_owned());
            if let SchemaRole::Other { type_name } = role {
                others.insert(type_name);
            }
        }
        Ok(Self {
            raw,
            classes,
            base: None,
            base_memo: None,
            items: BTreeMap::new(),
            memo: BTreeMap::new(),
            reserved,
            others,
        })
    }

    /// A builder for one operation's inline types, on top of a finished build.
    pub fn on_top_of(
        raw: &'a Value,
        classes: &'a Classes,
        items: &'a BTreeMap<String, Item>,
        memo: &'a BTreeMap<(String, Direction), Ty>,
    ) -> GenResult<Self> {
        let mut builder = Self::new(raw, classes)?;
        builder.base = Some(items);
        builder.base_memo = Some(memo);
        Ok(builder)
    }

    /// Builds every component schema in every direction it is used in.
    pub fn build_all(&mut self) -> GenResult<()> {
        let reached: Vec<_> = self.classes.reached.iter().cloned().collect();
        for (name, direction) in reached {
            self.component(&name, direction)?;
        }
        mark_boxes(&mut self.items);
        Ok(())
    }

    /// The type of `schema` as it appears in an operation, naming an inline object `hint`.
    pub fn operation_type(
        &mut self,
        schema: &'a Value,
        hint: &str,
        direction: Direction,
    ) -> GenResult<Ty> {
        let ctx = Ctx {
            direction,
            owner: String::new(),
            field: String::new(),
            home: None,
            description: schema::description(schema),
        };
        let ty = match schema::kind(schema) {
            Some("object") | None
                if schema::wrapped_ref(schema).is_none()
                    && schema::one_of(schema).is_empty()
                    && (schema.get("properties").is_some()
                        || !schema::all_of(schema).is_empty()) =>
            {
                let properties = schema::flatten(self.raw, schema)?;
                self.inline_struct(hint.to_owned(), &properties, &ctx)?
            }
            Some("string") if schema.get("enum").is_some() => {
                let values = enum_values(schema, hint)?;
                self.enum_named(hint.to_owned(), values, &ctx)?
            }
            _ => self.ty_of(schema, &ctx)?,
        };
        mark_boxes(&mut self.items);
        Ok(ty)
    }

    fn exists(&self, name: &str) -> bool {
        self.items.contains_key(name) || self.base.is_some_and(|base| base.contains_key(name))
    }

    fn item(&self, name: &str) -> Option<&Item> {
        self.items
            .get(name)
            .or_else(|| self.base.and_then(|base| base.get(name)))
    }

    fn insert(&mut self, item: Item) -> GenResult<()> {
        if self.exists(&item.name) {
            return Err(format!(
                "two generated types are both called `{}`",
                item.name
            ));
        }
        self.items.insert(item.name.clone(), item);
        Ok(())
    }

    /// The type a component schema is, building it first when needed.
    fn component(&mut self, name: &str, direction: Direction) -> GenResult<Ty> {
        let key = (name.to_owned(), direction);
        if let Some(ty) = self
            .memo
            .get(&key)
            .or_else(|| self.base_memo.and_then(|memo| memo.get(&key)))
        {
            return Ok(ty.clone());
        }
        let raw = self.raw;
        let body = schema::component(raw, name)?;
        match (role_of(name), direction) {
            (SchemaRole::Read { family }, Direction::Read) => {
                let ty = Ty::Named(family.clone());
                self.memo.insert(key, ty.clone());
                self.family(&family)?;
                Ok(ty)
            }
            (SchemaRole::Read { family }, Direction::Write) => {
                if self.classes.unions.contains(&family) {
                    return Err(format!("the union `{family}` is used in a request"));
                }
                let type_name = format!("{family}Input");
                let ty = Ty::Named(type_name.clone());
                self.memo.insert(key, ty.clone());
                if !self.exists(&type_name) {
                    let members = self.classes.families[&family].clone();
                    let properties = self.merged(&members)?;
                    let doc = format!(
                        "The writable fields of [`{family}`](crate::models::{family}), as a request sends them."
                    );
                    self.object(type_name, doc, Some(family), &properties, Direction::Write)?;
                }
                Ok(ty)
            }
            (SchemaRole::Write { type_name }, Direction::Write) => {
                let ty = Ty::Named(type_name.clone());
                self.memo.insert(key, ty.clone());
                let properties = schema::flatten(raw, body)?;
                let doc = schema::chain_description(raw, body)
                    .and_then(docs::summary)
                    .unwrap_or_else(|| {
                        format!("The body of a request, `{name}` in Asana's specification.")
                    });
                let home = self.home_of(&type_name, Direction::Write);
                self.object(type_name, doc, home, &properties, Direction::Write)?;
                Ok(ty)
            }
            (SchemaRole::Write { type_name }, Direction::Read) => Err(format!(
                "the request schema `{type_name}` is read from a response; give it a read role in naming.rs"
            )),
            (SchemaRole::Other { type_name }, direction) => {
                let type_name = if direction == Direction::Write && self.classes.is_read(name) {
                    format!("{type_name}Input")
                } else {
                    type_name
                };
                let ty = Ty::Named(type_name.clone());
                self.memo.insert(key, ty.clone());
                if !schema::one_of(body).is_empty() {
                    return Err(format!("`{name}` is a union outside a resource family"));
                }
                let properties = schema::flatten(raw, body)?;
                let doc = schema::chain_description(raw, body)
                    .and_then(docs::summary)
                    .unwrap_or_else(|| format!("`{name}` in Asana's specification."));
                let home = self.home_of(&type_name, direction);
                self.object(type_name, doc, home, &properties, direction)?;
                Ok(ty)
            }
        }
    }

    /// The properties of a family's shapes together, in the order the smallest shape gives them.
    fn merged(&self, members: &[String]) -> GenResult<Vec<(&'a str, &'a Value)>> {
        let raw = self.raw;
        let mut properties: Vec<(&'a str, &'a Value)> = Vec::new();
        for member in members {
            let body = schema::component(raw, member)?;
            for (name, property) in schema::flatten(raw, body)? {
                match properties
                    .iter_mut()
                    .find(|(existing, _)| *existing == name)
                {
                    Some(slot) => slot.1 = property,
                    None => properties.push((name, property)),
                }
            }
        }
        Ok(properties)
    }

    /// Builds a read family's struct, or its union.
    fn family(&mut self, family: &str) -> GenResult<()> {
        if self.exists(family) {
            return Ok(());
        }
        let members = self
            .classes
            .families
            .get(family)
            .ok_or_else(|| format!("`{family}` is not a resource family"))?
            .clone();
        let raw = self.raw;
        let doc = members
            .iter()
            .filter_map(|member| schema::component(raw, member).ok())
            .find_map(|body| schema::chain_description(raw, body))
            .and_then(docs::summary)
            .unwrap_or_else(|| format!("An Asana `{family}`."));
        if self.classes.unions.contains(family) {
            return self.union(family, doc, &members);
        }
        let properties = self.merged(&members)?;
        self.object(
            family.to_owned(),
            doc,
            Some(family.to_owned()),
            &properties,
            Direction::Read,
        )
    }

    /// Builds a struct and its fields. The item is inserted before its fields are built, so a
    /// field may refer back to it.
    fn object(
        &mut self,
        name: String,
        doc: String,
        home: Option<String>,
        properties: &[(&'a str, &'a Value)],
        direction: Direction,
    ) -> GenResult<()> {
        self.insert(Item {
            name: name.clone(),
            doc,
            home: home.clone(),
            kind: ItemKind::Struct {
                direction,
                fields: Vec::new(),
            },
        })?;
        // `gid` and `resource_type` first, as Asana writes them; the rest as the schemas list them.
        let mut properties = properties.to_vec();
        properties.sort_by_key(|(json, _)| match *json {
            "gid" => 0,
            "resource_type" => 1,
            _ => 2,
        });
        let mut fields: Vec<Field> = Vec::new();
        for (json, property) in properties {
            if let Some(field) = self.field(&name, home.as_ref(), direction, json, property)? {
                if fields.iter().any(|existing| existing.rust == field.rust) {
                    return Err(format!("`{name}` has two fields called `{}`", field.rust));
                }
                fields.push(field);
            }
        }
        self.set_fields(&name, |slot| *slot = fields)
    }

    /// Changes the fields of a struct this build made.
    fn set_fields(&mut self, name: &str, change: impl FnOnce(&mut Vec<Field>)) -> GenResult<()> {
        match self.items.get_mut(name) {
            Some(Item {
                kind: ItemKind::Struct { fields, .. },
                ..
            }) => {
                change(fields);
                Ok(())
            }
            _ => Err(format!("`{name}` is not a struct this build can change")),
        }
    }

    /// One field of a struct, or `None` for a read-only property of a request.
    fn field(
        &mut self,
        owner: &str,
        home: Option<&String>,
        direction: Direction,
        json: &str,
        property: &'a Value,
    ) -> GenResult<Option<Field>> {
        if direction == Direction::Write && schema::read_only(property) {
            return Ok(None);
        }
        let ctx = Ctx {
            direction,
            owner: owner.to_owned(),
            field: json.to_owned(),
            home: home.cloned(),
            description: schema::description(property),
        };
        let ty = self.ty_of(property, &ctx)?;
        let doc = ctx
            .description
            .and_then(docs::summary)
            .unwrap_or_else(|| format!("The `{json}` field."));
        Ok(Some(Field {
            json: json.to_owned(),
            rust: super::render::field_name(json),
            ty,
            doc,
            clearable: direction == Direction::Write && schema::nullable(property),
            boxed: false,
        }))
    }

    /// A reference to a resource read with a few more properties than its family declares
    /// (`allOf: [{$ref: GoalMetricBase}, {properties: {can_manage}}]`): the family, given the
    /// properties it lacked.
    fn extended_family(&mut self, property: &'a Value) -> GenResult<Option<Ty>> {
        let parts = schema::all_of(property);
        let mut refs = parts.iter().filter_map(schema::ref_name);
        let (Some(target), None) = (refs.next(), refs.next()) else {
            return Ok(None);
        };
        let SchemaRole::Read { family } = role_of(target) else {
            return Ok(None);
        };
        if self.classes.unions.contains(&family) {
            return Ok(None);
        }
        let ty = self.component(target, Direction::Read)?;
        let mut extras = Vec::new();
        for part in parts.iter().filter(|part| schema::ref_name(part).is_none()) {
            extras.extend(schema::flatten(self.raw, part)?);
        }
        let home = Some(family.clone());
        for (json, extra) in extras {
            let known = match self.item(&family) {
                Some(Item {
                    kind: ItemKind::Struct { fields, .. },
                    ..
                }) => fields.iter().any(|field| field.json == json),
                _ => return Err(format!("`{family}` is not a struct")),
            };
            if known {
                continue;
            }
            if let Some(field) = self.field(&family, home.as_ref(), Direction::Read, json, extra)? {
                self.set_fields(&family, |fields| fields.push(field))?;
            }
        }
        Ok(Some(ty))
    }

    /// Builds a read union of families, choosing the field that tells them apart.
    fn union(&mut self, family: &str, doc: String, members: &[String]) -> GenResult<()> {
        let raw = self.raw;
        let mut variants: Vec<String> = Vec::new();
        for member in members {
            for option in schema::one_of(schema::component(raw, member)?) {
                let target = schema::ref_name(option)
                    .ok_or_else(|| format!("`{member}` has a member that is not a reference"))?;
                let SchemaRole::Read { family: variant } = role_of(target) else {
                    return Err(format!("`{member}` has the non-resource member `{target}`"));
                };
                if !variants.contains(&variant) {
                    variants.push(variant);
                }
            }
        }
        for variant in &variants {
            self.family(variant)?;
        }
        let mut chosen = None;
        for pointer in DISCRIMINATORS {
            let tags: Vec<Option<String>> = variants
                .iter()
                .map(|variant| self.tag_of(variant, pointer))
                .collect::<GenResult<_>>()?;
            let distinct: BTreeSet<_> = tags.iter().flatten().collect();
            if tags.iter().all(Option::is_some) && distinct.len() == tags.len() {
                chosen = Some((*pointer, tags.into_iter().flatten().collect::<Vec<_>>()));
                break;
            }
        }
        let (pointer, tags) = chosen.ok_or_else(|| {
            format!("nothing tells the members of `{family}` apart: {variants:?}")
        })?;
        let strip = variants
            .iter()
            .all(|variant| variant.len() > family.len() && variant.ends_with(family));
        let variants = variants
            .into_iter()
            .zip(tags)
            .map(|(variant, tag)| UnionVariant {
                name: if strip {
                    variant[..variant.len() - family.len()].to_owned()
                } else {
                    variant.clone()
                },
                family: variant,
                tag,
            })
            .collect();
        self.insert(Item {
            name: family.to_owned(),
            doc,
            home: Some(family.to_owned()),
            kind: ItemKind::Union {
                discriminator: pointer.to_owned(),
                variants,
            },
        })
    }

    /// The literal a family gives at a discriminator pointer, following one reference.
    fn tag_of(&self, family: &str, pointer: &str) -> GenResult<Option<String>> {
        let members = &self.classes.families[family];
        let properties = self.merged(members)?;
        let mut steps = pointer.trim_start_matches('/').split('/');
        let Some(first) = steps.next() else {
            return Ok(None);
        };
        let Some((_, property)) = properties.iter().find(|(name, _)| *name == first) else {
            return Ok(None);
        };
        match steps.next() {
            None => Ok(schema::literal(property).map(str::to_owned)),
            Some(second) => {
                let Some(target) = schema::wrapped_ref(property) else {
                    return Ok(None);
                };
                let SchemaRole::Read { family: inner } = role_of(target) else {
                    return Ok(None);
                };
                let inner_properties = self.merged(&self.classes.families[&inner])?;
                Ok(inner_properties
                    .iter()
                    .find(|(name, _)| *name == second)
                    .and_then(|(_, property)| schema::literal(property))
                    .map(str::to_owned))
            }
        }
    }

    /// The file a type with no family of its own goes in: the longest family its name starts
    /// with; for a request, else the longest family it contains (`CreateTimeTrackingEntryRequest`);
    /// for anything else, a group of schemas sharing a name (`AuditLogEvent`, `AuditLogEventActor`)
    /// under the shortest of them. `None` puts it in the shared file.
    fn home_of(&self, type_name: &str, direction: Direction) -> Option<String> {
        let families = self.classes.families.keys();
        if let Some(family) = families
            .clone()
            .filter(|family| type_name.starts_with(family.as_str()))
            .max_by_key(|family| family.len())
        {
            return Some(family.clone());
        }
        if direction == Direction::Write {
            return families
                .filter(|family| type_name.contains(family.as_str()))
                .max_by_key(|family| family.len())
                .cloned();
        }
        let group: Vec<&String> = self
            .others
            .iter()
            .filter(|other| type_name.starts_with(other.as_str()) || other.starts_with(type_name))
            .collect();
        (group.len() > 1)
            .then(|| group.into_iter().min_by_key(|other| other.len()).cloned())
            .flatten()
    }

    /// The type of a property, an array element or a map value.
    fn ty_of(&mut self, property: &'a Value, ctx: &Ctx<'a>) -> GenResult<Ty> {
        if let Some(name) = schema::wrapped_ref(property) {
            return self.component(name, ctx.direction);
        }
        if !schema::one_of(property).is_empty() {
            return self.write_union(property, ctx);
        }
        if !schema::all_of(property).is_empty() {
            if ctx.direction == Direction::Read
                && let Some(ty) = self.extended_family(property)?
            {
                return Ok(ty);
            }
            let properties = schema::flatten(self.raw, property)?;
            return self.object_ty(property, properties, ctx);
        }
        match schema::kind(property) {
            Some("string") => self.string_ty(property, ctx),
            Some("integer") => Ok(Ty::Integer),
            Some("number") => Ok(Ty::Decimal),
            Some("boolean") => Ok(Ty::Bool),
            Some("array") => {
                let items = property.get("items").ok_or_else(|| {
                    format!("`{}.{}` is an array without items", ctx.owner, ctx.field)
                })?;
                let ctx = Ctx {
                    description: ctx.description.or_else(|| schema::description(property)),
                    ..ctx.clone()
                };
                Ok(Ty::Vec(Box::new(self.ty_of(items, &ctx)?)))
            }
            Some("object") | None => {
                let properties = schema::flatten(self.raw, property)?;
                self.object_ty(property, properties, ctx)
            }
            Some(other) => Err(format!(
                "`{}.{}` has the unsupported type `{other}`",
                ctx.owner, ctx.field
            )),
        }
    }

    fn object_ty(
        &mut self,
        property: &'a Value,
        properties: Vec<(&'a str, &'a Value)>,
        ctx: &Ctx<'a>,
    ) -> GenResult<Ty> {
        let additional = property.get("additionalProperties");
        if properties.is_empty() {
            let Some(additional) = additional else {
                return Ok(Ty::Json);
            };
            let value = if additional.is_object() {
                self.ty_of(additional, ctx)?
            } else {
                Ty::Json
            };
            let key = if matches!(ctx.field.as_str(), "custom_fields" | "html_custom_fields")
                && self.classes.gid_families.contains_key("CustomField")
            {
                Ty::Gid("CustomField".to_owned())
            } else {
                Ty::String
            };
            return Ok(Ty::Map(Box::new(key), Box::new(value)));
        }
        if additional.is_some_and(|additional| additional != &Value::Bool(false)) {
            return Ok(Ty::JsonObject);
        }
        if ctx.direction == Direction::Read
            && let Some(family) = self.compact_reference(&properties)?
        {
            self.family(&family)?;
            return Ok(Ty::Named(family));
        }
        let name = format!("{}{}", ctx.owner, ctx.field.to_upper_camel_case());
        self.inline_struct(name, &properties, ctx)
    }

    /// The family an inline object is a compact reference to: it has a `gid`, its `resource_type`
    /// names one family, and every property it has, that family has too.
    fn compact_reference(&self, properties: &[(&str, &Value)]) -> GenResult<Option<String>> {
        if !properties.iter().any(|(name, _)| *name == "gid") {
            return Ok(None);
        }
        let Some(family) = properties
            .iter()
            .find(|(name, _)| *name == "resource_type")
            .and_then(|(_, property)| schema::literal(property))
            .and_then(|literal| self.classes.family_of_resource_type.get(literal))
        else {
            return Ok(None);
        };
        let Some(members) = self.classes.families.get(family) else {
            return Ok(None);
        };
        let known = self.merged(members)?;
        let covered = properties
            .iter()
            .all(|(name, _)| known.iter().any(|(existing, _)| existing == name));
        Ok(covered.then(|| family.clone()))
    }

    fn inline_struct(
        &mut self,
        name: String,
        properties: &[(&'a str, &'a Value)],
        ctx: &Ctx<'a>,
    ) -> GenResult<Ty> {
        if self.reserved.contains(&name) {
            return Err(format!(
                "the inline object `{}.{}` would be called `{name}`, which a schema already is",
                ctx.owner, ctx.field
            ));
        }
        let doc = ctx.description.and_then(docs::summary).unwrap_or_else(|| {
            if ctx.owner.is_empty() {
                format!("`{name}`.")
            } else {
                format!("The `{}` of a `{}`.", ctx.field, ctx.owner)
            }
        });
        self.object(
            name.clone(),
            doc,
            ctx.home.clone(),
            properties,
            ctx.direction,
        )?;
        Ok(Ty::Named(name))
    }

    fn string_ty(&mut self, property: &'a Value, ctx: &Ctx<'a>) -> GenResult<Ty> {
        if property.get("enum").is_some() {
            let values = enum_values(property, &ctx.field)?;
            let home = ctx.home.clone().unwrap_or_else(|| ctx.owner.clone());
            let field = ctx.field.to_upper_camel_case();
            let candidates = [format!("{home}{field}"), format!("{}{field}", ctx.owner)];
            for candidate in candidates {
                match self.item(&candidate) {
                    Some(Item {
                        kind: ItemKind::Enum { values: existing },
                        ..
                    }) if same_values(existing, &values) => return Ok(Ty::Named(candidate)),
                    Some(_) => {}
                    None if !self.reserved.contains(&candidate) => {
                        return self.enum_named(candidate, values, ctx);
                    }
                    None => {}
                }
            }
            return Err(format!(
                "no free name for the enum `{}.{}`",
                ctx.owner, ctx.field
            ));
        }
        match property.get("format").and_then(Value::as_str) {
            Some("date") => return Ok(Ty::Date),
            Some("date-time") => return Ok(Ty::DateTime),
            Some("binary") => {
                return Err(format!(
                    "`{}.{}` is binary, which no JSON model can hold",
                    ctx.owner, ctx.field
                ));
            }
            _ => {}
        }
        let field = ctx.field.as_str();
        if field == "gid" {
            let family = match ctx.direction {
                Direction::Read => ctx.owner.as_str(),
                Direction::Write => ctx
                    .owner
                    .strip_suffix("Input")
                    .or_else(|| ctx.owner.strip_suffix("Request"))
                    .unwrap_or(&ctx.owner),
            };
            return Ok(self.classes.gid_type(family).map_or(Ty::String, Ty::Gid));
        }
        if let Some(prefix) = field
            .strip_suffix("_gid")
            .or_else(|| field.strip_suffix("_gids"))
            && let Some(family) = self.classes.gid_type(&prefix.to_upper_camel_case())
        {
            return Ok(Ty::Gid(family));
        }
        if ctx.direction == Direction::Write {
            let description = ctx
                .description
                .or_else(|| schema::description(property))
                .unwrap_or_default();
            if description.contains("\"me\", an email, or the gid of a user") {
                return Ok(Ty::UserRef);
            }
            // A request names a resource by its GID in a field called after its family
            // (`workspace`, `projects`), unless the field also takes some other identifier.
            if !description.contains("or globally-unique")
                && let Some(family) = [field, field.strip_suffix('s').unwrap_or(field)]
                    .into_iter()
                    .find_map(|name| self.classes.gid_type(&name.to_upper_camel_case()))
            {
                return Ok(Ty::Gid(family));
            }
        }
        Ok(Ty::String)
    }

    fn enum_named(&mut self, name: String, values: Vec<String>, ctx: &Ctx<'a>) -> GenResult<Ty> {
        if let Some(Item {
            kind: ItemKind::Enum { values: existing },
            ..
        }) = self.item(&name)
            && same_values(existing, &values)
        {
            return Ok(Ty::Named(name));
        }
        let doc = if ctx.owner.is_empty() {
            format!("A value Asana lists for `{name}`.")
        } else {
            format!("A value of `{}` on `{}`.", ctx.field, ctx.owner)
        };
        self.insert(Item {
            name: name.clone(),
            doc,
            home: ctx.home.clone(),
            kind: ItemKind::Enum { values },
        })?;
        Ok(Ty::Named(name))
    }

    /// A `oneOf` in a request: a GID or a full object, for example.
    fn write_union(&mut self, property: &'a Value, ctx: &Ctx<'a>) -> GenResult<Ty> {
        if ctx.direction == Direction::Read {
            return Err(format!(
                "`{}.{}` is an inline union in a response, which is not supported",
                ctx.owner, ctx.field
            ));
        }
        let name = format!("{}{}", ctx.owner, ctx.field.to_upper_camel_case());
        let mut variants = Vec::new();
        for option in schema::one_of(property) {
            let variant = if let Some(target) = schema::wrapped_ref(option) {
                let ty = self.component(target, Direction::Write)?;
                let label = ty.named().unwrap_or(target).to_owned();
                (label, ty)
            } else if schema::kind(option) == Some("string") {
                if let Some(family) = self.classes.gid_type(&ctx.field.to_upper_camel_case()) {
                    ("Gid".to_owned(), Ty::Gid(family))
                } else {
                    ("Text".to_owned(), Ty::String)
                }
            } else {
                return Err(format!(
                    "`{}.{}` has a union member that is neither a reference nor a string",
                    ctx.owner, ctx.field
                ));
            };
            variants.push(variant);
        }
        let doc = ctx
            .description
            .and_then(docs::summary)
            .unwrap_or_else(|| format!("What a request may send as `{}`.", ctx.field));
        self.insert(Item {
            name: name.clone(),
            doc,
            home: ctx.home.clone(),
            kind: ItemKind::WriteUnion { variants },
        })?;
        Ok(Ty::Named(name))
    }
}

fn enum_values(property: &Value, field: &str) -> GenResult<Vec<String>> {
    property
        .get("enum")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{field}` has an enum that is not a list"))?
        .iter()
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("`{field}` has a value that is not a string: {value}"))
        })
        .collect()
}

fn same_values(left: &[String], right: &[String]) -> bool {
    let left: BTreeSet<_> = left.iter().collect();
    let right: BTreeSet<_> = right.iter().collect();
    left == right
}

/// Boxes every field whose type, directly or through other structs, contains the struct that
/// holds it; without the box the struct would have no finite size. `Vec` and maps already put
/// their contents on the heap, and union variants are always boxed, so only plain struct fields
/// count.
pub fn mark_boxes(items: &mut BTreeMap<String, Item>) {
    let edges: BTreeMap<String, BTreeSet<String>> = items
        .iter()
        .filter_map(|(name, item)| match &item.kind {
            ItemKind::Struct { fields, .. } => Some((
                name.clone(),
                fields
                    .iter()
                    .filter_map(|field| field.ty.named())
                    .filter(|target| {
                        items
                            .get(*target)
                            .is_some_and(|item| matches!(item.kind, ItemKind::Struct { .. }))
                    })
                    .map(str::to_owned)
                    .collect(),
            )),
            _ => None,
        })
        .collect();
    let reaches = |from: &str, to: &str| -> bool {
        let mut seen = BTreeSet::new();
        let mut stack = vec![from.to_owned()];
        while let Some(next) = stack.pop() {
            if next == to {
                return true;
            }
            if seen.insert(next.clone())
                && let Some(targets) = edges.get(&next)
            {
                stack.extend(targets.iter().cloned());
            }
        }
        false
    };
    let mut boxed = Vec::new();
    for (owner, targets) in &edges {
        for target in targets {
            if reaches(target, owner) {
                boxed.push((owner.clone(), target.clone()));
            }
        }
    }
    for (owner, target) in boxed {
        if let Some(Item {
            kind: ItemKind::Struct { fields, .. },
            ..
        }) = items.get_mut(&owner)
        {
            for field in fields.iter_mut() {
                if field.ty.named() == Some(target.as_str()) {
                    field.boxed = true;
                }
            }
        }
    }
}
