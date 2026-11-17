//! Users: the token's own user, a user by GID or email, and a workspace's users.

use asana_openapi::endpoints::{GetUser, GetUserForWorkspace, GetUsersForWorkspace};
use asana_openapi::fields;
use asana_openapi::fields::UserField;
use asana_openapi::ids::{UserRef, WorkspaceGid};
use asana_openapi::models::User;

use super::request::{Request, opt_fields_setter};
use crate::{Client, Error};

impl Client {
    /// Users.
    pub fn users(&self) -> Users<'_> {
        Users { client: self }
    }
}

/// The users handle: `client.users()`.
#[derive(Clone, Copy, Debug)]
pub struct Users<'a> {
    client: &'a Client,
}

opt_fields_setter!(
    GetUser => UserField,
    GetUserForWorkspace => UserField,
    GetUsersForWorkspace => UserField,
);

impl<'a> Users<'a> {
    /// The user the token belongs to, with their name, email address and the workspaces they can
    /// reach (each with its name). Choose other fields with `.fields(..)`.
    ///
    /// `GET /users/me` (`getUser`); scope `users:read`; retried on a server error.
    pub fn me(&self) -> Request<'a, GetUser> {
        self.get(UserRef::Me)
            .fields(fields![User: name, email, workspaces, workspaces.name])
    }

    /// A user, by GID, by email address, or `me`.
    ///
    /// `GET /users/{user_gid}` (`getUser`); scope `users:read`; retried on a server error.
    pub fn get(&self, user: impl Into<UserRef>) -> Request<'a, GetUser> {
        Request::new(self.client, GetUser::builder(user).build())
    }

    /// The workspace member with this email address, or `None` when nobody matches.
    ///
    /// `GET /workspaces/{workspace_gid}/users/{email}` (`getUserForWorkspace`); scope
    /// `users:read`; retried on a server error. Asana answers an address it cannot match to a
    /// member it lets the token see with `404 Not Found`, which this returns as `Ok(None)`; it
    /// does not say whether the address belongs to nobody or to someone the token cannot see.
    /// Asana matches the address as the user's account holds it.
    ///
    /// # Errors
    ///
    /// Any other error, as [`Client::send`].
    pub async fn by_email(
        &self,
        workspace: &WorkspaceGid,
        email: &str,
    ) -> Result<Option<User>, Error> {
        let endpoint = GetUserForWorkspace::builder(workspace.clone(), UserRef::email(email))
            .opt_fields(fields![User: name, email])
            .build();
        match self.client.send(endpoint).await {
            Ok(user) => Ok(Some(user)),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// A workspace's users, as a list to stream or page.
    ///
    /// `GET /workspaces/{workspace_gid}/users` (`getUsersForWorkspace`); scope `users:read`;
    /// retried on a server error.
    pub fn list(&self, workspace: WorkspaceGid) -> Request<'a, GetUsersForWorkspace> {
        Request::new(
            self.client,
            GetUsersForWorkspace::builder(workspace).build(),
        )
    }
}
