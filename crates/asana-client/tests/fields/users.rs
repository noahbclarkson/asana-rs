//! The users handle: the token's own user, and a user by email.

use asana_client::Error;
use asana_client::openapi::ids::WorkspaceGid;

use crate::helpers::{param, path};
use crate::support::{Scripted, client, reply};

fn workspace() -> WorkspaceGid {
    "1201234567890100".parse().unwrap()
}

#[tokio::test(start_paused = true)]
async fn me_asks_for_the_users_workspaces() {
    let transport = Scripted::new(|_, _| {
        Ok(reply(
            200,
            r#"{"data": {"gid": "1201234567890061", "resource_type": "user", "name": "Ada Example",
                "email": "ada@example.com",
                "workspaces": [{"gid": "1201234567890100", "resource_type": "workspace", "name": "Acme"}]}}"#,
        ))
    });
    let client = client(transport.clone());

    let me = client.users().me().send().await.unwrap();
    assert_eq!(me.workspaces.unwrap()[0].name.as_deref(), Some("Acme"));
    let request = transport.request(0);
    assert_eq!(path(&request), "/users/me");
    assert_eq!(
        param(&request, "opt_fields").as_deref(),
        Some("name,email,workspaces,workspaces.name")
    );
}

#[tokio::test(start_paused = true)]
async fn by_email_finds_a_member_with_the_address_encoded_in_the_path() {
    let transport = Scripted::new(|_, _| {
        Ok(reply(
            200,
            r#"{"data": {"gid": "1201234567890061", "resource_type": "user", "name": "Ada Example", "email": "ada+ops@example.com"}}"#,
        ))
    });
    let client = client(transport.clone());

    let user = client
        .users()
        .by_email(&workspace(), "ada+ops@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.gid.unwrap().to_string(), "1201234567890061");
    assert_eq!(
        transport.request(0).uri().path(),
        "/api/1.0/workspaces/1201234567890100/users/ada%2Bops%40example%2Ecom"
    );
}

#[tokio::test(start_paused = true)]
async fn by_email_is_none_when_asana_answers_not_found() {
    let transport = Scripted::new(|_, _| {
        Ok(reply(
            404,
            r#"{"errors": [{"message": "user: Not a recognized ID: nobody@example.com", "help": "For more information on API status codes and how to handle them, read the docs on errors: https://developers.asana.com/docs/errors"}]}"#,
        ))
    });
    let client = client(transport);

    let user = client
        .users()
        .by_email(&workspace(), "nobody@example.com")
        .await
        .unwrap();
    assert!(user.is_none());
}

#[tokio::test(start_paused = true)]
async fn by_email_passes_other_errors_on() {
    let transport =
        Scripted::new(|_, _| Ok(reply(403, r#"{"errors": [{"message": "Forbidden"}]}"#)));
    let client = client(transport);

    let error = client
        .users()
        .by_email(&workspace(), "ada@example.com")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)));
}
