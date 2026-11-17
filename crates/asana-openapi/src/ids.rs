//! Typed Asana GIDs.
//!
//! Asana identifies every object by a GID: decimal digits sent as a string. Here each kind of
//! object has a type of its own, so the compiler refuses a section's GID where a task's is wanted.
//! A GID is validated when it is made (1 to 64 ASCII digits), which also means one can never carry
//! a `/` or a `?` into a request path.
//!
//! The types themselves are listed in the generated code and re-exported here.

use std::fmt;

pub use crate::generated::ids::*;

/// A typed Asana GID.
pub trait Gid: Clone + fmt::Display + Send + Sync + 'static {
    /// Asana's `resource_type` for the objects this GID names, for example `"task"`.
    const RESOURCE_TYPE: &'static str;

    /// The GID as Asana writes it.
    fn as_str(&self) -> &str;

    /// Validates `text` as a GID of this kind.
    ///
    /// # Errors
    ///
    /// [`GidError`] when `text` is empty, longer than 64 characters or not all ASCII digits.
    fn parse(text: &str) -> Result<Self, GidError>;
}

/// Why a piece of text is not an Asana GID.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GidError {
    /// The text was empty.
    #[error("an Asana GID cannot be empty")]
    Empty,
    /// The text was longer than any GID Asana issues.
    #[error("an Asana GID has at most 64 digits, and this text has {0} characters")]
    TooLong(usize),
    /// The text held something other than ASCII digits.
    #[error("an Asana GID holds only the digits 0 to 9")]
    NotDigits,
}

/// A user as a request names one: the token's own user, an email address, or a GID.
///
/// Asana accepts all three wherever a request names a user, for example in `/users/{user_gid}`
/// and in a task's `assignee`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UserRef {
    /// The user the token belongs to (`me`).
    Me,
    /// A user by email address.
    Email(String),
    /// A user by GID.
    Gid(UserGid),
}

impl UserRef {
    /// A user by email address.
    pub fn email(address: impl Into<String>) -> Self {
        Self::Email(address.into())
    }
}

impl From<UserGid> for UserRef {
    fn from(gid: UserGid) -> Self {
        Self::Gid(gid)
    }
}

/// Writes `me`, the email address or the GID, exactly as Asana expects it in a JSON body. In a
/// path or a query it must be percent-encoded by whoever places it there.
impl fmt::Display for UserRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Me => f.write_str("me"),
            Self::Email(address) => f.write_str(address),
            Self::Gid(gid) => f.write_str(gid.as_str()),
        }
    }
}

impl serde::Serialize for UserRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Checks the one rule every GID follows. Used by the types the [`gid!`](crate::gid) macro makes.
#[doc(hidden)]
pub fn validate(text: &str) -> Result<(), GidError> {
    if text.is_empty() {
        return Err(GidError::Empty);
    }
    if text.len() > 64 {
        return Err(GidError::TooLong(text.len()));
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(GidError::NotDigits);
    }
    Ok(())
}

/// Defines typed GIDs: `gid!(TaskGid => "task", SectionGid => "section");`.
///
/// Each becomes a cheap-to-clone newtype over `Arc<str>` that implements [`Gid`], `FromStr`,
/// `TryFrom<String>`, `Display`, `Debug` (as `TaskGid(1201)`), ordering, hashing and serde (as a
/// plain JSON string, validated when read).
#[macro_export]
macro_rules! gid {
    ($($(#[$meta:meta])* $name:ident => $resource_type:literal),* $(,)?) => { $(
        $(#[$meta])*
        #[doc = concat!("The GID of an Asana `", $resource_type, "`.")]
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(::std::sync::Arc<str>);

        impl $crate::ids::Gid for $name {
            const RESOURCE_TYPE: &'static str = $resource_type;

            fn as_str(&self) -> &str {
                &self.0
            }

            fn parse(text: &str) -> ::std::result::Result<Self, $crate::ids::GidError> {
                $crate::ids::validate(text)?;
                Ok(Self(::std::sync::Arc::from(text)))
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::ids::GidError;

            fn from_str(text: &str) -> ::std::result::Result<Self, Self::Err> {
                <Self as $crate::ids::Gid>::parse(text)
            }
        }

        impl ::std::convert::TryFrom<String> for $name {
            type Error = $crate::ids::GidError;

            fn try_from(text: String) -> ::std::result::Result<Self, Self::Error> {
                <Self as $crate::ids::Gid>::parse(&text)
            }
        }

        impl ::std::convert::From<$name> for String {
            fn from(gid: $name) -> Self {
                gid.0.as_ref().to_owned()
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), &self.0)
            }
        }
    )* };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gid_is_digits_only_and_round_trips_as_a_json_string() {
        let task: TaskGid = "1201234567890123".parse().unwrap();
        assert_eq!(task.as_str(), "1201234567890123");
        assert_eq!(format!("{task:?}"), "TaskGid(1201234567890123)");
        assert_eq!(
            serde_json::to_string(&task).unwrap(),
            "\"1201234567890123\""
        );
        assert_eq!(
            serde_json::from_str::<TaskGid>("\"1201234567890123\"").unwrap(),
            task
        );
    }

    #[test]
    fn text_that_could_alter_a_path_is_refused() {
        assert_eq!(TaskGid::parse(""), Err(GidError::Empty));
        assert_eq!(TaskGid::parse("12/../34"), Err(GidError::NotDigits));
        assert_eq!(TaskGid::parse("12?x=1"), Err(GidError::NotDigits));
        assert_eq!(TaskGid::parse(&"1".repeat(65)), Err(GidError::TooLong(65)));
        assert!(serde_json::from_str::<TaskGid>("\"me\"").is_err());
    }
}
