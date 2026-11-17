//! Stories: comments on a task and the activity Asana records on it.
//!
//! Asana lists a task's stories oldest first; [`Stories::latest`] reads the most recent ones
//! without walking the whole history.
//!
//! ```no_run
//! # async fn run(client: asana_client::Client, task: asana_client::openapi::ids::TaskGid)
//! #     -> Result<(), asana_client::Error> {
//! use asana_client::Html;
//! use asana_client::resources::stories::StoryExt;
//!
//! client.stories().comment(task.clone(), Html::paragraph("Approved and ").bold("sent")).send().await?;
//! for story in client.stories().latest(task, 5).send().await? {
//!     if story.is_comment() {
//!         println!("{}", story.text.unwrap_or_default());
//!     }
//! }
//! # Ok(()) }
//! ```

use asana_openapi::endpoints::{
    CreateStoryForTask, DeleteStory, GetStoriesForTask, GetStoriesForTaskResourceSubtype, GetStory,
    UpdateStory,
};
use asana_openapi::fields::{Fields, StoryField};
use asana_openapi::ids::{StoryGid, TaskGid};
use asana_openapi::models::{
    Story, StoryRequest, StoryType, TaskStickerName, TaskStoryCreateRequest,
};

use crate::resources::request::{Request, opt_fields_setter};
use crate::{Client, Error, Html};

impl Client {
    /// Stories: comment on a task and read its comments and activity.
    pub fn stories(&self) -> Stories<'_> {
        Stories { client: self }
    }
}

/// The story operations, from [`Client::stories`].
#[derive(Clone, Copy, Debug)]
pub struct Stories<'a> {
    client: &'a Client,
}

opt_fields_setter!(
    CreateStoryForTask => StoryField,
    GetStoriesForTask => StoryField,
    GetStory => StoryField,
    UpdateStory => StoryField,
);

/// What a comment says: plain text, or rich text built with [`Html`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommentText {
    /// Plain text, sent as `text`. Asana shows it as written, markup included.
    Plain(String),
    /// Rich text, sent as `html_text`.
    Html(Html),
}

impl From<&str> for CommentText {
    fn from(text: &str) -> Self {
        Self::Plain(text.to_owned())
    }
}

impl From<String> for CommentText {
    fn from(text: String) -> Self {
        Self::Plain(text)
    }
}

impl From<Html> for CommentText {
    fn from(html: Html) -> Self {
        Self::Html(html)
    }
}

impl<'a> Stories<'a> {
    /// Adds a comment to a task.
    ///
    /// Sends `createStoryForTask` (`POST /tasks/{task_gid}/stories`); scope `stories:write`; never
    /// retried after a failure that leaves the outcome unknown, because a second send would post
    /// the comment twice.
    pub fn comment(
        &self,
        task: TaskGid,
        text: impl Into<CommentText>,
    ) -> Request<'a, CreateStoryForTask> {
        let data = match text.into() {
            CommentText::Plain(text) => TaskStoryCreateRequest::builder().text(text).build(),
            CommentText::Html(html) => TaskStoryCreateRequest::builder()
                .html_text(html.to_string())
                .build(),
        };
        Request::new(
            self.client,
            CreateStoryForTask::builder(task).data(data).build(),
        )
    }

    /// A task's stories, comments and activity together, oldest first, as a list
    /// (`.stream()`, `.collect_max(n)`).
    ///
    /// Sends `getStoriesForTask` (`GET /tasks/{task_gid}/stories`), one page of 100 at a time;
    /// scope `stories:read`; retried like every read.
    pub fn list(&self, task: TaskGid) -> Request<'a, GetStoriesForTask> {
        Request::new(self.client, GetStoriesForTask::builder(task).build())
    }

    /// The last `count` stories on a task, returned oldest first like [`list`](Self::list).
    ///
    /// Asks Asana for the newest first (`sort_ascending=false`), so it costs one request per
    /// hundred stories wanted however long the task's history is: `count` itself is the bound on
    /// pages read. Asana notes that on an extremely long activity feed the feed may be truncated
    /// before it is ordered; [`created_after`](Latest::created_after) bounds what it considers.
    ///
    /// Sends `getStoriesForTask` (`GET /tasks/{task_gid}/stories?sort_ascending=false`); scope
    /// `stories:read`; retried like every read.
    pub fn latest(&self, task: TaskGid, count: usize) -> Latest<'a> {
        Latest {
            client: self.client,
            endpoint: GetStoriesForTask::builder(task)
                .sort_ascending(false)
                .build(),
            count,
        }
    }

    /// One story.
    ///
    /// Sends `getStory` (`GET /stories/{story_gid}`); scope `stories:read`; retried like every
    /// read.
    pub fn get(&self, story: StoryGid) -> Request<'a, GetStory> {
        Request::new(self.client, GetStory::builder(story).build())
    }

    /// Changes a comment's text, or pins or unpins it. Only comments the token's user wrote can
    /// be edited.
    ///
    /// Sends `updateStory` (`PUT /stories/{story_gid}`); scope `stories:write`; retried on a
    /// server failure (a `PUT` is safe to repeat).
    pub fn update(&self, story: StoryGid) -> Request<'a, UpdateStory> {
        Request::new(
            self.client,
            UpdateStory::builder(story)
                .data(StoryRequest::default())
                .build(),
        )
    }

    /// Deletes a comment the token's user wrote.
    ///
    /// Sends `deleteStory` (`DELETE /stories/{story_gid}`); scope `stories:delete`; retried on a
    /// server failure, and a `404` to a retry after an attempt whose answer was lost counts as
    /// deleted.
    pub fn delete(&self, story: StoryGid) -> Request<'a, DeleteStory> {
        Request::new(self.client, DeleteStory::builder(story).build())
    }
}

impl Request<'_, CreateStoryForTask> {
    /// Pins the comment to the top of the task.
    pub fn pinned(self, pinned: bool) -> Self {
        self.map(|endpoint| endpoint.data.is_pinned = Some(pinned))
    }

    /// Adds a sticker to the comment.
    pub fn sticker(self, sticker: TaskStickerName) -> Self {
        self.map(|endpoint| endpoint.data.sticker_name = Some(sticker))
    }

    /// Posts the comment as a reply in a comment thread. `comment_gid` is the thread's
    /// [`Story::comment_gid`], not a story's `gid`.
    pub fn reply_to(self, comment_gid: impl Into<String>) -> Self {
        let parent = comment_gid.into();
        self.map(|endpoint| endpoint.data.parent_comment = Some(parent))
    }
}

impl Request<'_, GetStoriesForTask> {
    /// Only comments, without the activity Asana records.
    pub fn comments_only(self) -> Self {
        self.map(|endpoint| {
            endpoint.resource_subtype = Some(GetStoriesForTaskResourceSubtype::CommentAdded);
        })
    }

    /// Only stories created after this time. Asana truncates it to the second, so the story at
    /// the boundary may be returned again: de-duplicate on `gid`.
    pub fn created_after(self, after: time::OffsetDateTime) -> Self {
        self.map(|endpoint| endpoint.created_after = Some(after))
    }
}

impl Request<'_, UpdateStory> {
    /// Replaces the comment's text with plain text.
    pub fn text(self, text: impl Into<String>) -> Self {
        let text = text.into();
        self.map(|endpoint| {
            endpoint.data.html_text = None;
            endpoint.data.text = Some(text);
        })
    }

    /// Replaces the comment's text with rich text.
    pub fn html(self, html: &Html) -> Self {
        let html = html.to_string();
        self.map(|endpoint| {
            endpoint.data.text = None;
            endpoint.data.html_text = Some(html);
        })
    }

    /// Pins or unpins the comment.
    pub fn pinned(self, pinned: bool) -> Self {
        self.map(|endpoint| endpoint.data.is_pinned = Some(pinned))
    }
}

/// The request [`Stories::latest`] returns.
#[must_use = "a request does nothing until it is sent"]
#[derive(Debug)]
pub struct Latest<'a> {
    client: &'a Client,
    endpoint: GetStoriesForTask,
    count: usize,
}

impl Latest<'_> {
    /// Chooses the fields Asana returns (`opt_fields`).
    pub fn fields(mut self, fields: Fields<StoryField>) -> Self {
        self.endpoint.opt_fields = fields;
        self
    }

    /// Only comments, without the activity Asana records.
    pub fn comments_only(mut self) -> Self {
        self.endpoint.resource_subtype = Some(GetStoriesForTaskResourceSubtype::CommentAdded);
        self
    }

    /// Only stories created after this time.
    pub fn created_after(mut self, after: time::OffsetDateTime) -> Self {
        self.endpoint.created_after = Some(after);
        self
    }

    /// Reads the stories and returns them oldest first.
    ///
    /// # Errors
    ///
    /// As [`Client::send`].
    pub async fn send(self) -> Result<Vec<Story>, Error> {
        if self.count == 0 {
            return Ok(Vec::new());
        }
        let mut stories = self.client.collect_max(self.endpoint, self.count).await?;
        stories.reverse();
        Ok(stories)
    }
}

/// Reading what kind of story a [`Story`] is.
pub trait StoryExt {
    /// Whether it is a comment a person (or an app) wrote, rather than activity Asana recorded.
    ///
    /// Reads `resource_subtype` (`comment_added`) or `type` (`comment`); a story read without
    /// either field is neither a comment nor activity, so request `resource_subtype` when it
    /// matters.
    fn is_comment(&self) -> bool;

    /// Whether it is activity Asana recorded (an assignment, a due date change, a move), rather
    /// than a comment. The same field caveat as [`is_comment`](Self::is_comment) applies.
    fn is_activity(&self) -> bool;
}

impl StoryExt for Story {
    fn is_comment(&self) -> bool {
        self.resource_subtype.as_deref() == Some("comment_added")
            || self.r#type == Some(StoryType::Comment)
    }

    fn is_activity(&self) -> bool {
        if self.is_comment() {
            return false;
        }
        self.r#type == Some(StoryType::System) || self.resource_subtype.is_some()
    }
}
