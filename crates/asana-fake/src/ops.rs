//! The handlers, one module per family of operations, and the table of every route served.

pub(crate) mod attachments;
pub(crate) mod batch;
pub(crate) mod directory;
pub(crate) mod events;
pub(crate) mod fields;
pub(crate) mod projects;
pub(crate) mod search;
pub(crate) mod sections;
pub(crate) mod stories;
pub(crate) mod task_links;
pub(crate) mod tasks;
pub(crate) mod templates;
pub(crate) mod webhooks;

use crate::router::Route;

/// Every operation the fake serves.
pub(crate) fn routes() -> Vec<Route> {
    [
        tasks::routes(),
        task_links::routes(),
        sections::routes(),
        projects::routes(),
        fields::routes(),
        directory::routes(),
        search::routes(),
        stories::routes(),
        attachments::routes(),
        templates::routes(),
        events::routes(),
        webhooks::routes(),
        batch::routes(),
    ]
    .into_iter()
    .flatten()
    .collect()
}
