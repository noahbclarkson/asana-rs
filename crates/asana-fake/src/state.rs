//! The fake's whole state: every record, the GID counter, the clock, and what the transport keeps
//! between requests (recorded calls, queued faults, the receiver).
//!
//! Records are stored by GID in ordered maps. GIDs come from one counter and all have sixteen
//! digits, so a map's order is creation order. The ordering of tasks inside a project or section,
//! of sections in a project and of subtasks is kept in explicit vectors, because Asana lets a
//! caller insert before or after a sibling.

use std::collections::{BTreeMap, HashMap, VecDeque};

use asana_webhooks::Clock;
use bytes::Bytes;
use serde_json::Value;
use time::OffsetDateTime;

use crate::answer::Refusal;
use crate::faults::QueuedFault;
use crate::recorded::RecordedRequest;
use crate::webhook_delivery::Receiver;

/// A GID as the fake stores it: sixteen ASCII digits.
pub(crate) type Id = String;

/// What kind of object a GID names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Workspace,
    User,
    Project,
    Section,
    Task,
    CustomField,
    EnumOption,
    CustomFieldSetting,
    Tag,
    Story,
    Attachment,
    TaskTemplate,
    Job,
    Webhook,
}

impl Kind {
    /// Asana's `resource_type` for this kind.
    pub(crate) const fn resource_type(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::User => "user",
            Self::Project => "project",
            Self::Section => "section",
            Self::Task => "task",
            Self::CustomField => "custom_field",
            Self::EnumOption => "enum_option",
            Self::CustomFieldSetting => "custom_field_setting",
            Self::Tag => "tag",
            Self::Story => "story",
            Self::Attachment => "attachment",
            Self::TaskTemplate => "task_template",
            Self::Job => "job",
            Self::Webhook => "webhook",
        }
    }
}

/// A workspace (or organisation).
#[derive(Clone, Debug)]
pub(crate) struct Workspace {
    pub(crate) name: String,
    /// Whether the workspace is on a paid plan; search answers `402` when it is not.
    pub(crate) premium: bool,
    pub(crate) users: Vec<Id>,
}

/// A person.
#[derive(Clone, Debug)]
pub(crate) struct User {
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) workspaces: Vec<Id>,
}

/// A project, with its sections in order and the tasks that are in no section.
#[derive(Clone, Debug)]
pub(crate) struct Project {
    pub(crate) name: String,
    pub(crate) workspace: Id,
    pub(crate) notes: String,
    pub(crate) archived: bool,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) modified_at: OffsetDateTime,
    pub(crate) sections: Vec<Id>,
    /// Tasks in the project but in none of its sections (only while it has no sections).
    pub(crate) loose: Vec<Id>,
    pub(crate) settings: Vec<Id>,
}

/// A section, with its tasks in order.
#[derive(Clone, Debug)]
pub(crate) struct Section {
    pub(crate) name: String,
    pub(crate) project: Id,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) tasks: Vec<Id>,
}

/// A task's place in one project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Membership {
    pub(crate) project: Id,
    pub(crate) section: Option<Id>,
}

/// A value of one custom field on a task.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FieldValue {
    Text(String),
    Number(serde_json::Number),
    Enum(Id),
    MultiEnum(Vec<Id>),
    Date {
        date: String,
        date_time: Option<String>,
    },
    People(Vec<Id>),
    Reference(Vec<Id>),
}

/// A task.
#[derive(Clone, Debug)]
pub(crate) struct Task {
    pub(crate) name: String,
    pub(crate) resource_subtype: String,
    pub(crate) notes: String,
    pub(crate) html_notes: Option<String>,
    pub(crate) workspace: Id,
    pub(crate) completed: bool,
    pub(crate) completed_at: Option<OffsetDateTime>,
    pub(crate) due_on: Option<String>,
    pub(crate) due_at: Option<String>,
    pub(crate) start_on: Option<String>,
    pub(crate) start_at: Option<String>,
    pub(crate) assignee: Option<Id>,
    pub(crate) created_by: Option<Id>,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) modified_at: OffsetDateTime,
    pub(crate) memberships: Vec<Membership>,
    pub(crate) parent: Option<Id>,
    pub(crate) subtasks: Vec<Id>,
    pub(crate) followers: Vec<Id>,
    pub(crate) tags: Vec<Id>,
    pub(crate) dependencies: Vec<Id>,
    pub(crate) values: BTreeMap<Id, FieldValue>,
}

/// The kinds of custom field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Text,
    Number,
    Enum,
    MultiEnum,
    Date,
    People,
    Reference,
}

impl FieldKind {
    /// Asana's `resource_subtype` (and `type`) for the kind.
    pub(crate) const fn subtype(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Enum => "enum",
            Self::MultiEnum => "multi_enum",
            Self::Date => "date",
            Self::People => "people",
            Self::Reference => "reference",
        }
    }

    /// Reads Asana's `resource_subtype`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "text" => Self::Text,
            "number" => Self::Number,
            "enum" => Self::Enum,
            "multi_enum" => Self::MultiEnum,
            "date" => Self::Date,
            "people" => Self::People,
            "reference" => Self::Reference,
            _ => return None,
        })
    }
}

/// A custom field's definition.
#[derive(Clone, Debug)]
pub(crate) struct CustomField {
    pub(crate) name: String,
    pub(crate) kind: FieldKind,
    pub(crate) workspace: Id,
    pub(crate) description: String,
    pub(crate) precision: u32,
    pub(crate) options: Vec<Id>,
}

/// One option of an enum or multi-enum field.
#[derive(Clone, Debug)]
pub(crate) struct EnumOption {
    pub(crate) field: Id,
    pub(crate) name: String,
    pub(crate) color: String,
    pub(crate) enabled: bool,
}

/// A custom field added to a project.
#[derive(Clone, Debug)]
pub(crate) struct Setting {
    pub(crate) project: Id,
    pub(crate) field: Id,
    pub(crate) important: bool,
}

/// A tag.
#[derive(Clone, Debug)]
pub(crate) struct Tag {
    pub(crate) name: String,
    pub(crate) workspace: Id,
    pub(crate) color: Option<String>,
    pub(crate) notes: String,
    pub(crate) created_at: OffsetDateTime,
}

/// A story: a comment or a record of a change.
#[derive(Clone, Debug)]
pub(crate) struct Story {
    pub(crate) task: Id,
    pub(crate) created_by: Option<Id>,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) resource_subtype: String,
    pub(crate) text: String,
    pub(crate) html_text: Option<String>,
    pub(crate) pinned: bool,
    pub(crate) edited: bool,
    /// For a section move: the sections it moved from and to.
    pub(crate) old_section: Option<Id>,
    pub(crate) new_section: Option<Id>,
}

impl Story {
    /// Whether the story is a comment (rather than a record the system made).
    pub(crate) fn is_comment(&self) -> bool {
        self.resource_subtype == "comment_added"
    }
}

/// An attachment's content.
#[derive(Clone, Debug)]
pub(crate) enum Content {
    /// A file uploaded to the fake.
    File { content_type: String, bytes: Bytes },
    /// A link to a file kept elsewhere.
    External { url: String },
}

/// An attachment on a task or project.
#[derive(Clone, Debug)]
pub(crate) struct Attachment {
    pub(crate) name: String,
    pub(crate) parent: Id,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) content: Content,
}

/// A task template in a project.
#[derive(Clone, Debug)]
pub(crate) struct TaskTemplate {
    pub(crate) name: String,
    pub(crate) project: Id,
    pub(crate) task_name: String,
    pub(crate) description: String,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) created_by: Option<Id>,
}

/// The state of a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobStatus {
    InProgress,
    Succeeded,
}

/// A job: instantiating a template or duplicating a task.
#[derive(Clone, Debug)]
pub(crate) struct Job {
    pub(crate) subtype: &'static str,
    pub(crate) status: JobStatus,
    /// How many more reads answer `in_progress` before the job succeeds.
    pub(crate) polls_left: u32,
    pub(crate) new_task: Option<Id>,
}

/// A webhook and the events waiting to be delivered to it.
#[derive(Clone, Debug)]
pub(crate) struct Webhook {
    pub(crate) resource: Id,
    pub(crate) workspace: Id,
    pub(crate) target: String,
    pub(crate) filters: Vec<Value>,
    pub(crate) secret: String,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) last_success_at: Option<OffsetDateTime>,
    pub(crate) last_failure_at: Option<OffsetDateTime>,
    pub(crate) last_failure_content: Option<String>,
    pub(crate) delivery_retry_count: u32,
    pub(crate) pending: VecDeque<Value>,
}

/// One events stream (`GET /events`), created by its first `412`.
#[derive(Clone, Debug)]
pub(crate) struct Stream {
    pub(crate) resource: Id,
    pub(crate) events: Vec<Value>,
}

/// Everything the fake holds.
pub(crate) struct State {
    pub(crate) clock: Clock,
    next_gid: u64,
    next_token: u64,
    pub(crate) kinds: HashMap<Id, Kind>,
    pub(crate) workspaces: BTreeMap<Id, Workspace>,
    pub(crate) users: BTreeMap<Id, User>,
    pub(crate) projects: BTreeMap<Id, Project>,
    pub(crate) sections: BTreeMap<Id, Section>,
    pub(crate) tasks: BTreeMap<Id, Task>,
    pub(crate) fields: BTreeMap<Id, CustomField>,
    pub(crate) options: BTreeMap<Id, EnumOption>,
    pub(crate) settings: BTreeMap<Id, Setting>,
    pub(crate) tags: BTreeMap<Id, Tag>,
    pub(crate) stories: BTreeMap<Id, Story>,
    pub(crate) attachments: BTreeMap<Id, Attachment>,
    pub(crate) templates: BTreeMap<Id, TaskTemplate>,
    pub(crate) jobs: BTreeMap<Id, Job>,
    pub(crate) webhooks: BTreeMap<Id, Webhook>,
    pub(crate) streams: Vec<Stream>,
    /// Sync token to (stream index, how many of its events were already returned).
    pub(crate) sync_tokens: HashMap<String, (usize, usize)>,
    /// The user the token belongs to (`me`), made on first need when no user was seeded.
    pub(crate) me: Option<Id>,
    /// Tokens and their users; while empty, any bearer token is `me`.
    pub(crate) tokens: HashMap<String, Id>,
    /// Who is acting right now: the token's user during a request, the actor's user for
    /// [`FakeAsana::as_user`](crate::FakeAsana::as_user).
    pub(crate) actor: Option<Id>,
    /// While set (seeding), changes make no events and no stories.
    pub(crate) silent: bool,
    pub(crate) requests: Vec<RecordedRequest>,
    pub(crate) faults: VecDeque<QueuedFault>,
    pub(crate) receiver: Option<Receiver>,
    pub(crate) auto_deliver: bool,
    /// How many reads of a new job answer `in_progress` before it succeeds.
    pub(crate) job_polls: u32,
}

/// The first GID the counter hands out, less one: sixteen digits, like Asana's.
const GID_BASE: u64 = 1_200_000_000_000_000;

impl State {
    pub(crate) fn new(clock: Clock) -> Self {
        Self {
            clock,
            next_gid: 0,
            next_token: 0,
            kinds: HashMap::new(),
            workspaces: BTreeMap::new(),
            users: BTreeMap::new(),
            projects: BTreeMap::new(),
            sections: BTreeMap::new(),
            tasks: BTreeMap::new(),
            fields: BTreeMap::new(),
            options: BTreeMap::new(),
            settings: BTreeMap::new(),
            tags: BTreeMap::new(),
            stories: BTreeMap::new(),
            attachments: BTreeMap::new(),
            templates: BTreeMap::new(),
            jobs: BTreeMap::new(),
            webhooks: BTreeMap::new(),
            streams: Vec::new(),
            sync_tokens: HashMap::new(),
            me: None,
            tokens: HashMap::new(),
            actor: None,
            silent: false,
            requests: Vec::new(),
            faults: VecDeque::new(),
            receiver: None,
            auto_deliver: false,
            job_polls: 0,
        }
    }

    /// The fake's current time.
    pub(crate) fn now(&self) -> OffsetDateTime {
        (self.clock)()
    }

    /// Hands out the next GID for an object of `kind`.
    pub(crate) fn new_gid(&mut self, kind: Kind) -> Id {
        self.next_gid += 1;
        let gid = (GID_BASE + self.next_gid).to_string();
        self.kinds.insert(gid.clone(), kind);
        gid
    }

    /// Hands out an opaque token (sync tokens, webhook secrets): 32 hex digits, deterministic.
    pub(crate) fn new_token(&mut self) -> String {
        self.next_token += 1;
        // A fixed odd multiplier spreads consecutive counters over the whole range, so tokens do
        // not look sequential (and a test cannot lean on their order).
        let mixed = self.next_token.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        format!(
            "{mixed:016x}{:016x}",
            mixed.rotate_left(29) ^ self.next_token
        )
    }

    /// What kind of object `gid` names, if it names one.
    pub(crate) fn kind_of(&self, gid: &str) -> Option<Kind> {
        self.kinds.get(gid).copied()
    }

    /// The user acting now: the actor if one is set, otherwise the token's user.
    pub(crate) fn acting_user(&mut self) -> Id {
        match self.actor.clone() {
            Some(actor) => actor,
            None => self.me(),
        }
    }

    /// The token's user, making a default one if nobody was seeded.
    pub(crate) fn me(&mut self) -> Id {
        if let Some(me) = &self.me {
            return me.clone();
        }
        let workspaces: Vec<Id> = self.workspaces.keys().cloned().collect();
        let gid = self.new_gid(Kind::User);
        for workspace in &workspaces {
            if let Some(record) = self.workspaces.get_mut(workspace) {
                record.users.push(gid.clone());
            }
        }
        self.users.insert(
            gid.clone(),
            User {
                name: "Fake User".to_owned(),
                email: "me@example.com".to_owned(),
                workspaces,
            },
        );
        self.me = Some(gid.clone());
        gid
    }

    /// The display name of the object `gid` names, for compact references.
    pub(crate) fn name_of(&self, gid: &str) -> Option<&str> {
        let name = match self.kind_of(gid)? {
            Kind::Workspace => &self.workspaces.get(gid)?.name,
            Kind::User => &self.users.get(gid)?.name,
            Kind::Project => &self.projects.get(gid)?.name,
            Kind::Section => &self.sections.get(gid)?.name,
            Kind::Task => &self.tasks.get(gid)?.name,
            Kind::CustomField => &self.fields.get(gid)?.name,
            Kind::EnumOption => &self.options.get(gid)?.name,
            Kind::Tag => &self.tags.get(gid)?.name,
            Kind::Attachment => &self.attachments.get(gid)?.name,
            Kind::TaskTemplate => &self.templates.get(gid)?.name,
            Kind::CustomFieldSetting | Kind::Story | Kind::Job | Kind::Webhook => return None,
        };
        Some(name)
    }

    /// Whether `gid` names an object that still exists.
    pub(crate) fn exists(&self, gid: &str) -> bool {
        match self.kind_of(gid) {
            Some(Kind::Workspace) => self.workspaces.contains_key(gid),
            Some(Kind::User) => self.users.contains_key(gid),
            Some(Kind::Project) => self.projects.contains_key(gid),
            Some(Kind::Section) => self.sections.contains_key(gid),
            Some(Kind::Task) => self.tasks.contains_key(gid),
            Some(Kind::CustomField) => self.fields.contains_key(gid),
            Some(Kind::EnumOption) => self.options.contains_key(gid),
            Some(Kind::CustomFieldSetting) => self.settings.contains_key(gid),
            Some(Kind::Tag) => self.tags.contains_key(gid),
            Some(Kind::Story) => self.stories.contains_key(gid),
            Some(Kind::Attachment) => self.attachments.contains_key(gid),
            Some(Kind::TaskTemplate) => self.templates.contains_key(gid),
            Some(Kind::Job) => self.jobs.contains_key(gid),
            Some(Kind::Webhook) => self.webhooks.contains_key(gid),
            None => false,
        }
    }

    /// Resolves a user named as a GID, an email address or `me`.
    pub(crate) fn resolve_user(&mut self, param: &str, reference: &str) -> Result<Id, Refusal> {
        if reference == "me" {
            return Ok(self.acting_user());
        }
        if reference.contains('@') {
            return self
                .users
                .iter()
                .find(|(_, user)| user.email.eq_ignore_ascii_case(reference))
                .map(|(gid, _)| gid.clone())
                .ok_or_else(|| Refusal::not_found(param, reference));
        }
        if self.users.contains_key(reference) {
            Ok(reference.to_owned())
        } else {
            Err(Refusal::unknown(param, reference))
        }
    }

    /// The workspace an object belongs to, where it has one.
    pub(crate) fn workspace_of(&self, gid: &str) -> Option<Id> {
        match self.kind_of(gid)? {
            Kind::Workspace => Some(gid.to_owned()),
            Kind::Project => self.projects.get(gid).map(|p| p.workspace.clone()),
            Kind::Section => self
                .sections
                .get(gid)
                .and_then(|s| self.workspace_of(&s.project)),
            Kind::Task => self.tasks.get(gid).map(|t| t.workspace.clone()),
            Kind::Tag => self.tags.get(gid).map(|t| t.workspace.clone()),
            Kind::CustomField => self.fields.get(gid).map(|f| f.workspace.clone()),
            Kind::Story => self
                .stories
                .get(gid)
                .and_then(|s| self.workspace_of(&s.task)),
            Kind::User => self.users.get(gid)?.workspaces.first().cloned(),
            _ => None,
        }
    }

    /// The custom fields shown on a task: those of every project it is in, then any other field
    /// it holds a value for.
    pub(crate) fn fields_on_task(&self, task: &Task) -> Vec<Id> {
        let mut fields: Vec<Id> = Vec::new();
        for membership in &task.memberships {
            if let Some(project) = self.projects.get(&membership.project) {
                for setting in &project.settings {
                    if let Some(setting) = self.settings.get(setting)
                        && !fields.contains(&setting.field)
                    {
                        fields.push(setting.field.clone());
                    }
                }
            }
        }
        for field in task.values.keys() {
            if !fields.contains(field) {
                fields.push(field.clone());
            }
        }
        fields
    }
}

/// Looks up a record by GID, refusing with Asana's `404` when it is missing.
pub(crate) fn lookup<'a, T>(
    map: &'a BTreeMap<Id, T>,
    param: &str,
    gid: &str,
) -> Result<&'a T, Refusal> {
    map.get(gid).ok_or_else(|| Refusal::unknown(param, gid))
}

/// Looks up a record by GID for changing it, refusing with Asana's `404` when it is missing.
pub(crate) fn lookup_mut<'a, T>(
    map: &'a mut BTreeMap<Id, T>,
    param: &str,
    gid: &str,
) -> Result<&'a mut T, Refusal> {
    map.get_mut(gid).ok_or_else(|| Refusal::unknown(param, gid))
}
