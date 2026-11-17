#![forbid(unsafe_code)]

//! Receiving Asana webhooks, with no HTTP client and no web framework.
//!
//! Asana calls a webhook's target twice over: once with a handshake that must be echoed while the
//! creating request is still open, and from then on with signed batches of compact events. This
//! crate is that protocol as plain functions over headers and bytes: tell the two kinds of call
//! apart, answer the handshake, verify a delivery's signature over its raw bytes, and decode the
//! events into types.
