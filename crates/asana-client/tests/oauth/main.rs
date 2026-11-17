//! OAuth against a scripted transport: the authorisation URL, PKCE, the token calls' requests and
//! answers, the refreshing token source (with a clock the tests move), scopes, and secrets kept
//! out of `Debug` and `Display`.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers fail the test loudly on an impossible setup"
)]

#[path = "../pipeline/support.rs"]
#[expect(
    dead_code,
    reason = "shared with the pipeline tests, which use the parts these do not"
)]
mod support;

mod fixtures;

mod authorize;
mod calls;
mod scopes;
mod secrets;
mod source;
