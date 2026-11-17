//! Builds a client with every option set, then reads a task and tells the errors apart.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...   a personal access token
//! ASANA_TASK_GID=...       a task to read
//! cargo run -p asana-client --example client_setup
//! ```

use std::env;
use std::time::Duration;

use asana_client::openapi::ids::TaskGid;
use asana_client::{Auth, Client, Error, Limits, ReqwestTransport, RetryPolicy, fields};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let task: TaskGid = env::var("ASANA_TASK_GID")?.parse()?;

    // Your own reqwest client keeps your TLS, proxy and timeout choices. Turn redirects off:
    // Asana's API does not redirect, and a redirect would carry the token elsewhere.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .build()?;

    let client = Client::builder()
        // Or `Auth::service_account(..)`, or `Auth::oauth(..)` with a token source.
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .transport(ReqwestTransport::new(http).max_body(8 * 1024 * 1024))
        // Sent as "acme-sync/2.1 asana-rs/<version>".
        .user_agent("acme-sync/2.1")
        // Up to 3 sends of one request, never waiting more than 30 seconds between them.
        .retry(
            RetryPolicy::builder()
                .max_attempts(3)
                .max_wait(Duration::from_secs(30))
                .build(),
        )
        // Below Asana's own per-token limits, leaving room for another tool on the same token.
        .limits(
            Limits::builder()
                .reads(20)
                .writes(5)
                .searches_per_minute(30)
                .build(),
        )
        // Opt in early to an announced API change (`Asana-Enable`).
        .asana_enable(["new_user_task_lists"])
        .build()?;

    match client
        .tasks()
        .get(task)
        .fields(fields![Task: name])
        .send()
        .await
    {
        Ok(read) => println!("{:?}", read.name),
        // Asana's own messages are kept to read on purpose; `Display` and `Debug` never print them.
        Err(Error::NotFound(errors)) => {
            println!("no such task: {:?}", errors.messages().collect::<Vec<_>>());
        }
        Err(Error::Forbidden(_)) => println!("the token cannot see that task"),
        // Every send already waited out `Retry-After`; this is after the last attempt.
        Err(Error::RateLimited {
            retry_after,
            attempts,
        }) => {
            println!("still limited after {attempts} sends; Asana asks for {retry_after:?}");
        }
        Err(other) if other.is_retryable() => println!("worth trying later: {other}"),
        Err(other) => return Err(other.into()),
    }

    // Deprecations Asana announced on responses (`Asana-Change`), once per change.
    for change in client.observed_changes() {
        println!(
            "Asana is changing {} (affects us: {})",
            change.name, change.affected
        );
    }
    Ok(())
}
