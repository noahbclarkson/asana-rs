//! Reading request headers without depending on any HTTP library.
//!
//! [`classify`](crate::classify) needs two headers and nothing else, so it reads them through
//! [`HeaderLookup`], a one-method trait. It is implemented here for the plain containers a test or
//! a small server has to hand (slices and vectors of pairs, arrays, `HashMap`s, and a function
//! through [`FnHeaders`]); the framework adapters implement it for their own header maps.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::marker::PhantomData;

/// Asana's handshake header: the webhook's secret, sent once while the webhook is being created.
pub const HOOK_SECRET: &str = "X-Hook-Secret";

/// Asana's delivery header: the hex HMAC-SHA256 of the raw body under the webhook's secret.
pub const HOOK_SIGNATURE: &str = "X-Hook-Signature";

/// Looks up a request header by name.
///
/// The contract: `name` is matched **case-insensitively** (HTTP header names are), and when a
/// header appears more than once any one of its values may be returned. A value that is not valid
/// UTF-8 is reported as absent; Asana's two headers are always ASCII.
pub trait HeaderLookup {
    /// The value of the header called `name`, if the request carries it.
    fn get(&self, name: &str) -> Option<&str>;
}

impl<T: HeaderLookup + ?Sized> HeaderLookup for &T {
    fn get(&self, name: &str) -> Option<&str> {
        (**self).get(name)
    }
}

impl<K: AsRef<str>, V: AsRef<str>> HeaderLookup for [(K, V)] {
    fn get(&self, name: &str) -> Option<&str> {
        self.iter()
            .find(|(key, _)| key.as_ref().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_ref())
    }
}

impl<K: AsRef<str>, V: AsRef<str>, const N: usize> HeaderLookup for [(K, V); N] {
    fn get(&self, name: &str) -> Option<&str> {
        HeaderLookup::get(self.as_slice(), name)
    }
}

impl<K: AsRef<str>, V: AsRef<str>> HeaderLookup for Vec<(K, V)> {
    fn get(&self, name: &str) -> Option<&str> {
        HeaderLookup::get(self.as_slice(), name)
    }
}

impl<K: AsRef<str>, V: AsRef<str>, S: BuildHasher> HeaderLookup for HashMap<K, V, S> {
    fn get(&self, name: &str) -> Option<&str> {
        // Keys may be stored in any case, so this scans rather than hashing `name`.
        self.iter()
            .find(|(key, _)| key.as_ref().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_ref())
    }
}

/// A [`HeaderLookup`] backed by a function, for header stores this crate does not know.
///
/// The function is handed the name exactly as asked for and must match it case-insensitively
/// itself. Made by [`FnHeaders::new`].
///
/// ```
/// use asana_webhooks::{FnHeaders, HeaderLookup};
///
/// let stored = [("x-hook-secret".to_owned(), "abc".to_owned())];
/// let headers = FnHeaders::new(|name: &str| {
///     stored
///         .iter()
///         .find(|(key, _)| key.eq_ignore_ascii_case(name))
///         .map(|(_, value)| value.as_str())
/// });
/// assert_eq!(headers.get("X-Hook-Secret"), Some("abc"));
/// ```
pub struct FnHeaders<'h, F> {
    lookup: F,
    borrowed: PhantomData<&'h str>,
}

impl<'h, F: Fn(&str) -> Option<&'h str>> FnHeaders<'h, F> {
    /// Wraps `lookup`, which returns header values borrowed from storage that outlives it.
    pub fn new(lookup: F) -> Self {
        Self {
            lookup,
            borrowed: PhantomData,
        }
    }
}

impl<'h, F: Fn(&str) -> Option<&'h str>> HeaderLookup for FnHeaders<'h, F> {
    fn get(&self, name: &str) -> Option<&str> {
        (self.lookup)(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_container_matches_names_in_any_case() {
        // Called through the trait: slices and maps have inherent `get` methods of their own.
        let pairs = [("x-hook-SIGNATURE", "aa")];
        assert_eq!(HeaderLookup::get(&pairs, HOOK_SIGNATURE), Some("aa"));
        assert_eq!(
            HeaderLookup::get(pairs.as_slice(), "X-HOOK-SIGNATURE"),
            Some("aa")
        );
        assert_eq!(HeaderLookup::get(&pairs, HOOK_SECRET), None);

        let owned = vec![("X-Hook-Secret".to_owned(), "s".to_owned())];
        assert_eq!(HeaderLookup::get(&owned, "x-hook-secret"), Some("s"));

        let map: HashMap<&str, &str> = HashMap::from([("X-HOOK-SECRET", "s")]);
        assert_eq!(HeaderLookup::get(&map, "x-hook-secret"), Some("s"));
        assert_eq!(HeaderLookup::get(&&map, "x-hook-signature"), None);
    }
}
