//! The client's one execution path, tested against a scripted transport (where time matters,
//! with tokio's clock paused) and against `wiremock` (where the real HTTP stack matters).

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers fail the test loudly on an impossible setup"
)]

mod support;

mod auth;
mod decoding;
mod errors;
mod limits;
mod paging;
mod requests;
mod retry;
