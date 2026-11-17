#![forbid(unsafe_code)]

//! An in-memory Asana for tests.
//!
//! A stateful fake behind `asana-client`'s transport: projects, sections, tasks, custom fields,
//! tags, stories and webhooks, answering with Asana's own envelope and error shapes. It performs
//! the webhook handshake and signs deliveries, so a consumer can test "a task moved to a section
//! starts my work" without a network or a token.
