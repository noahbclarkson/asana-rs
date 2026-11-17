//! Questions about a task that read its memberships: which section it is in, in which project.

use asana_openapi::ids::{ProjectGid, SectionGid};
use asana_openapi::models::{Section, Task};

/// Reading a [`Task`]'s place in its projects.
///
/// These read `memberships`, which Asana returns only when asked: request
/// `fields![Task: memberships.project, memberships.section]` (add `.name` to either for names).
/// On a task read without them, every answer is "none".
pub trait TaskExt {
    /// The section the task is in within `project`, if the task is in that project.
    fn section_in(&self, project: &ProjectGid) -> Option<&Section>;

    /// Every section the task is in, one per project that has sections.
    fn sections(&self) -> impl Iterator<Item = &Section>;

    /// Whether the task is in `section`.
    fn is_in_section(&self, section: &SectionGid) -> bool;

    /// The link that opens the task in Asana (`permalink_url`, returned when asked for).
    fn permalink(&self) -> Option<&str>;
}

impl TaskExt for Task {
    fn section_in(&self, project: &ProjectGid) -> Option<&Section> {
        self.memberships
            .iter()
            .flatten()
            .find(|membership| {
                membership
                    .project
                    .as_ref()
                    .and_then(|p| p.gid.as_ref())
                    .is_some_and(|gid| gid == project)
            })
            .and_then(|membership| membership.section.as_deref())
    }

    fn sections(&self) -> impl Iterator<Item = &Section> {
        self.memberships
            .iter()
            .flatten()
            .filter_map(|membership| membership.section.as_deref())
    }

    fn is_in_section(&self, section: &SectionGid) -> bool {
        self.sections()
            .any(|candidate| candidate.gid.as_ref() == Some(section))
    }

    fn permalink(&self) -> Option<&str> {
        self.permalink_url.as_deref()
    }
}
