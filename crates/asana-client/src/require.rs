//! Turning a field the caller cannot do without into an error when Asana left it out.
//!
//! Every field of a generated model is optional, because `opt_fields` decides what Asana
//! returns. Code that needs one writes `task.name.require("name")?` (or
//! `task.name.as_ref().require("name")?` to borrow) and gets [`Error::MissingField`] instead of a
//! panic.

use crate::error::Error;

/// A value that may be missing from a response.
pub trait Require<T> {
    /// The value, or [`Error::MissingField`] naming `field`.
    ///
    /// # Errors
    ///
    /// [`Error::MissingField`] when the value is absent.
    fn require(self, field: &'static str) -> Result<T, Error>;
}

impl<T> Require<T> for Option<T> {
    fn require(self, field: &'static str) -> Result<T, Error> {
        self.ok_or(Error::MissingField(field))
    }
}
