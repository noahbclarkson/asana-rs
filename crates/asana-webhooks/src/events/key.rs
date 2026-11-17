//! A stable identity for an event, which Asana's events do not carry.
//!
//! The key is SHA-256 over a canonical encoding of the fields that say what happened: the user's
//! GID, the resource's and the parent's kind and GID, the action, `created_at` to the nanosecond,
//! and the change's field, action and values (as JSON with its object keys sorted). Names and
//! subtypes are left out, because they describe the resource rather than the event and can be
//! rewritten. So a redelivery of the same event has the same key however its JSON is spaced or
//! ordered, and two events that differ in any of those fields have different keys.

use std::fmt;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{ChangeValue, Event, ResourceRef};

/// Version tag for the encoding. Changing the encoding changes every key, so it is versioned.
const DOMAIN: &[u8] = b"asana-webhooks event key v1\0";

/// A SHA-256 identity for an [`Event`], stable across redeliveries; made by [`Event::key`].
///
/// Use it to process each event at most once ([`Dedupe`](crate::Dedupe)) or as a primary key when
/// storing events. `Display` is 64 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventKey([u8; 32]);

impl EventKey {
    /// The key's 32 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// A key from 32 stored bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// A key from the 64 hex digits its `Display` writes. `None` for anything else.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        let mut bytes = [0_u8; 32];
        hex::decode_to_slice(text, &mut bytes).ok()?;
        Some(Self(bytes))
    }
}

impl fmt::Display for EventKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for EventKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EventKey({self})")
    }
}

impl Event {
    /// This event's [`EventKey`].
    #[must_use]
    pub fn key(&self) -> EventKey {
        let mut encoder = Encoder(Sha256::new());
        encoder.0.update(DOMAIN);
        encoder.field(
            self.user
                .as_ref()
                .map(|user| user.gid.to_string().into_bytes()),
        );
        encoder.resource(Some(&self.resource));
        encoder.resource(self.parent.as_ref());
        encoder.field(Some(self.action.as_str().as_bytes().to_vec()));
        encoder.field(Some(
            self.created_at
                .unix_timestamp_nanos()
                .to_string()
                .into_bytes(),
        ));
        match &self.change {
            None => encoder.field(None),
            Some(change) => {
                encoder.field(Some(change.field.as_bytes().to_vec()));
                encoder.field(Some(change.action.as_str().as_bytes().to_vec()));
                for value in [
                    &change.new_value,
                    &change.added_value,
                    &change.removed_value,
                ] {
                    encoder.field(value.as_ref().map(ChangeValue::canonical));
                }
            }
        }
        EventKey(encoder.0.finalize().into())
    }
}

impl ChangeValue {
    fn canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_canonical(&self.raw, &mut out);
        out
    }
}

/// Feeds fields into the hash so that no two different sequences of fields collide: each field is
/// absent (`0`) or present (`1`, its length as eight bytes, its bytes).
struct Encoder(Sha256);

impl Encoder {
    fn field(&mut self, bytes: Option<Vec<u8>>) {
        match bytes {
            None => self.0.update([0]),
            Some(bytes) => {
                self.0.update([1]);
                self.0.update((bytes.len() as u64).to_be_bytes());
                self.0.update(&bytes);
            }
        }
    }

    fn resource(&mut self, resource: Option<&ResourceRef>) {
        match resource {
            None => self.field(None),
            Some(resource) => {
                self.field(Some(resource.gid.resource_type().as_bytes().to_vec()));
                self.field(Some(resource.gid.gid_str().as_bytes().to_vec()));
            }
        }
    }
}

/// JSON with object keys sorted and no whitespace. The key order is decided here, not by
/// `serde_json`'s map, whose order depends on which cargo features are enabled.
fn write_canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(flag) => out.extend_from_slice(if *flag { b"true" } else { b"false" }),
        Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push(b'[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        Value::Object(object) => {
            let mut entries: Vec<(&String, &Value)> = object.iter().collect();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            out.push(b'{');
            for (position, (key, item)) in entries.into_iter().enumerate() {
                if position > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                write_canonical(item, out);
            }
            out.push(b'}');
        }
    }
}

fn write_string(text: &str, out: &mut Vec<u8>) {
    // A JSON string literal; `serde_json`'s escaping does not depend on any feature.
    match serde_json::to_vec(text) {
        Ok(encoded) => out.extend_from_slice(&encoded),
        Err(_) => out.extend_from_slice(text.as_bytes()),
    }
}
