//! `opt_fields`: which members of a record an answer carries.
//!
//! Every record is first described as [`Node`]s (a plain value, a reference to another record, a
//! list, or a plain nested object), and the answer is written from that description:
//!
//! - **No `opt_fields`, one object** (a get, a create, an update): the full record, with every
//!   reference to another record written compactly as `{gid, resource_type, name}`.
//! - **No `opt_fields`, a list**: each item compact (`gid`, `resource_type`, `name` for most
//!   resources), as Asana answers.
//! - **With `opt_fields`**: `gid` and exactly the paths asked for. A reference asked for by name
//!   alone is `{gid, resource_type}`; a nested path such as `memberships.section.name` reads into
//!   the referenced record.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::render;
use crate::state::{Id, Kind, State};

/// One member of a record, as the selection sees it.
#[derive(Clone, Debug)]
pub(crate) enum Node {
    /// A value written as it is.
    Value(Value),
    /// A reference to another record.
    Ref(Id),
    /// A list.
    List(Vec<Node>),
    /// A nested object that is not a record of its own (a membership, a custom field value).
    Object(Fields),
}

/// A record's members, in the order they are written.
pub(crate) type Fields = Vec<(&'static str, Node)>;

impl Node {
    /// A string value.
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self::Value(Value::String(text.into()))
    }

    /// A string value, or `null`.
    pub(crate) fn maybe_text(text: Option<&String>) -> Self {
        Self::Value(text.map_or(Value::Null, |text| Value::String(text.clone())))
    }

    /// A reference, or `null`.
    pub(crate) fn maybe_ref(gid: Option<&Id>) -> Self {
        gid.map_or(Self::Value(Value::Null), |gid| Self::Ref(gid.clone()))
    }

    /// A list of references.
    pub(crate) fn refs<'a>(gids: impl IntoIterator<Item = &'a Id>) -> Self {
        Self::List(gids.into_iter().cloned().map(Self::Ref).collect())
    }
}

/// The paths asked for with `opt_fields`, as a tree: `memberships.section.name` and
/// `memberships.project` share the `memberships` branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tree {
    children: BTreeMap<String, Tree>,
}

impl Tree {
    /// A tree of the given dotted paths.
    pub(crate) fn of<'a>(paths: impl IntoIterator<Item = &'a str>) -> Self {
        let mut tree = Self::default();
        for path in paths {
            let mut node = &mut tree;
            for segment in path.split('.').filter(|segment| !segment.is_empty()) {
                node = node.children.entry(segment.to_owned()).or_default();
            }
        }
        tree
    }

    fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }
}

/// What a request asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    fields: Option<Tree>,
}

impl Selection {
    /// Reads the `opt_fields` query value (comma-separated paths).
    pub(crate) fn parse(opt_fields: Option<&str>) -> Self {
        Self {
            fields: opt_fields.map(|text| Tree::of(text.split(',').map(str::trim))),
        }
    }

    /// The selection the batch API's `options.fields` names.
    pub(crate) fn of_list(fields: &[String]) -> Self {
        Self {
            fields: Some(Tree::of(fields.iter().map(String::as_str))),
        }
    }

    /// Whether any fields were asked for.
    pub(crate) fn is_set(&self) -> bool {
        self.fields.is_some()
    }

    /// Writes one record as a single-object answer: the full record unless fields were asked for.
    pub(crate) fn one(&self, state: &State, gid: &str) -> Value {
        let Some(fields) = render::record(state, gid) else {
            return Value::Null;
        };
        match &self.fields {
            Some(tree) => select(state, &fields, tree),
            None => full(state, &fields),
        }
    }

    /// Writes one record as a list item: compact unless fields were asked for.
    pub(crate) fn item(&self, state: &State, gid: &str) -> Value {
        let Some(fields) = render::record(state, gid) else {
            return Value::Null;
        };
        match &self.fields {
            Some(tree) => select(state, &fields, tree),
            None => {
                let kind = state.kind_of(gid).unwrap_or(Kind::Task);
                select(
                    state,
                    &fields,
                    &Tree::of(compact_fields(kind).iter().copied()),
                )
            }
        }
    }

    /// Writes a plain object that is not a record (task counts), keeping only the asked fields.
    pub(crate) fn object(&self, state: &State, fields: &Fields) -> Value {
        match &self.fields {
            Some(tree) => select(state, fields, tree),
            None => full(state, fields),
        }
    }
}

/// The members of a compact record of each kind, as Asana's `…Compact` schemas list them.
fn compact_fields(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Story => &[
            "gid",
            "resource_type",
            "created_at",
            "created_by",
            "resource_subtype",
            "text",
        ],
        Kind::Attachment => &["gid", "resource_type", "name", "resource_subtype"],
        Kind::EnumOption => &["gid", "resource_type", "name", "enabled", "color"],
        Kind::CustomField => &[
            "gid",
            "resource_type",
            "name",
            "resource_subtype",
            "type",
            "enum_options",
            "enabled",
            "precision",
        ],
        Kind::CustomFieldSetting => &[
            "gid",
            "resource_type",
            "custom_field",
            "project",
            "is_important",
        ],
        Kind::Job => &[
            "gid",
            "resource_type",
            "resource_subtype",
            "status",
            "new_task",
            "new_project",
        ],
        Kind::Webhook => &["gid", "resource_type", "active", "resource", "target"],
        Kind::Task => &["gid", "resource_type", "name", "resource_subtype"],
        Kind::Workspace
        | Kind::User
        | Kind::Project
        | Kind::Section
        | Kind::Tag
        | Kind::TaskTemplate => &["gid", "resource_type", "name"],
    }
}

/// The full rendering: every member, references compact with their names.
pub(crate) fn full(state: &State, fields: &Fields) -> Value {
    let mut out = Map::new();
    for (key, node) in fields {
        out.insert((*key).to_owned(), full_node(state, node));
    }
    Value::Object(out)
}

fn full_node(state: &State, node: &Node) -> Value {
    match node {
        Node::Value(value) => value.clone(),
        Node::Ref(gid) => named_ref(state, gid),
        Node::List(items) => {
            Value::Array(items.iter().map(|item| full_node(state, item)).collect())
        }
        Node::Object(fields) => full(state, fields),
    }
}

/// `{gid, resource_type, name}`, the reference Asana writes inside a full record.
pub(crate) fn named_ref(state: &State, gid: &str) -> Value {
    let mut out = bare_map(state, gid);
    if let Some(name) = state.name_of(gid) {
        out.insert("name".into(), Value::String(name.to_owned()));
    }
    Value::Object(out)
}

/// `{gid, resource_type}`, the reference Asana writes for a field asked for by name alone.
pub(crate) fn bare_ref(state: &State, gid: &str) -> Value {
    Value::Object(bare_map(state, gid))
}

fn bare_map(state: &State, gid: &str) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("gid".into(), Value::String(gid.to_owned()));
    if let Some(kind) = state.kind_of(gid) {
        out.insert(
            "resource_type".into(),
            Value::String(kind.resource_type().to_owned()),
        );
    }
    out
}

/// Keeps `gid` and the members `tree` names.
fn select(state: &State, fields: &Fields, tree: &Tree) -> Value {
    let mut out = Map::new();
    if let Some((_, gid)) = fields.iter().find(|(key, _)| *key == "gid") {
        out.insert("gid".into(), full_node(state, gid));
    }
    for (name, branch) in &tree.children {
        if let Some((key, node)) = fields.iter().find(|(key, _)| *key == name) {
            out.insert((*key).to_owned(), select_node(state, node, branch));
        }
    }
    Value::Object(out)
}

fn select_node(state: &State, node: &Node, tree: &Tree) -> Value {
    match node {
        Node::Value(value) if tree.is_leaf() => value.clone(),
        Node::Value(value) => select_value(value, tree),
        Node::Ref(gid) if tree.is_leaf() => bare_ref(state, gid),
        Node::Ref(gid) => match render::record(state, gid) {
            Some(fields) => select(state, &fields, tree),
            None => bare_ref(state, gid),
        },
        Node::List(items) => Value::Array(
            items
                .iter()
                .map(|item| select_node(state, item, tree))
                .collect(),
        ),
        Node::Object(fields) if tree.is_leaf() => full(state, fields),
        Node::Object(fields) => select(state, fields, tree),
    }
}

/// Selects inside a plain JSON value (`date_value.date`).
fn select_value(value: &Value, tree: &Tree) -> Value {
    match value {
        Value::Object(object) => {
            let mut out = Map::new();
            for (name, branch) in &tree.children {
                if let Some(member) = object.get(name) {
                    let member = if branch.is_leaf() {
                        member.clone()
                    } else {
                        select_value(member, branch)
                    };
                    out.insert(name.clone(), member);
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| select_value(item, tree)).collect())
        }
        other => other.clone(),
    }
}
