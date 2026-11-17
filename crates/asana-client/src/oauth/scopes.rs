//! Which OAuth scopes an app should ask for, and whether a token has the ones a call needs.
//!
//! Each generated descriptor names the granular scope its operation needs in
//! [`Endpoint::SCOPE`]. About two in five operations have none yet; Asana's guide says those are
//! reached only with **Full permissions**, the `default` scope, and that an app registered for Full
//! permissions cannot also ask for granular scopes. So a [`ScopeSet`] built from descriptors holds
//! either granular scopes or `default`, never both. Asana's scopes do not imply each other
//! (`write` does not grant `read`).
//!
//! ```
//! use asana_client::oauth::ScopeSet;
//! use asana_client::openapi::endpoints::{GetTask, UpdateTask};
//! use asana_client::openapi::scopes::Scope;
//!
//! let wanted = asana_client::scopes_for![GetTask, UpdateTask];
//! assert!(wanted.contains(Scope::TasksRead) && wanted.contains(Scope::TasksWrite));
//!
//! let granted = ScopeSet::parse("tasks:read projects:read");
//! assert_eq!(granted.missing(&wanted).to_string(), "tasks:write");
//! ```

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use asana_openapi::endpoint::Endpoint;
use asana_openapi::scopes::Scope;

/// The granular scope `E` needs, or `None` when the operation has none and needs Full permissions
/// (`default`).
pub fn required_scope<E: Endpoint>() -> Option<Scope> {
    E::SCOPE
}

/// The scopes an app asks for, or a token carries, kept in order.
///
/// Scopes this crate does not know (Asana adds them) are kept as text, so a set read from Asana
/// writes back exactly what it read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScopeSet {
    known: BTreeSet<Scope>,
    unknown: BTreeSet<Box<str>>,
}

impl ScopeSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads Asana's space-separated form, such as an introspection answer's `scope`. Repeated
    /// spaces are ignored.
    pub fn parse(text: &str) -> Self {
        let mut set = Self::new();
        for name in text.split_ascii_whitespace() {
            match name.parse::<Scope>() {
                Ok(scope) => {
                    set.known.insert(scope);
                }
                Err(_) => {
                    set.unknown.insert(name.into());
                }
            }
        }
        set
    }

    /// Adds `scope`.
    pub fn insert(&mut self, scope: Scope) {
        self.known.insert(scope);
    }

    /// The set with `scope` added.
    #[must_use]
    pub fn with(mut self, scope: Scope) -> Self {
        self.insert(scope);
        self
    }

    /// The set with what `E` needs added: its granular scope, or, when it has none, `default`
    /// (Full permissions) in place of every granular API scope. The OpenID Connect scopes
    /// (`openid`, `email`, `profile`) are kept either way: Asana does not document whether an app
    /// with Full permissions may also ask for them.
    #[must_use]
    pub fn with_endpoint<E: Endpoint>(mut self) -> Self {
        match E::SCOPE {
            Some(scope) => {
                if !self.known.contains(&Scope::Default) {
                    self.known.insert(scope);
                }
            }
            None => {
                self.known.retain(|scope| is_identity(*scope));
                self.unknown.clear();
                self.known.insert(Scope::Default);
            }
        }
        self
    }

    /// Whether `scope` is in the set, exactly.
    pub fn contains(&self, scope: Scope) -> bool {
        self.known.contains(&scope)
    }

    /// Whether a token with these scopes may use what `scope` grants: it is in the set, or the set
    /// holds `default` and `scope` is not an OpenID Connect scope.
    pub fn allows(&self, scope: Scope) -> bool {
        self.contains(scope) || (self.contains(Scope::Default) && !is_identity(scope))
    }

    /// The scopes in `required` that these scopes do not [allow](Self::allows). Unknown scopes in
    /// `required` count as missing unless this set holds the same text.
    pub fn missing(&self, required: &Self) -> Self {
        Self {
            known: required
                .known
                .iter()
                .copied()
                .filter(|scope| !self.allows(*scope))
                .collect(),
            unknown: required
                .unknown
                .difference(&self.unknown)
                .cloned()
                .collect(),
        }
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.known.is_empty() && self.unknown.is_empty()
    }

    /// How many scopes the set holds.
    pub fn len(&self) -> usize {
        self.known.len() + self.unknown.len()
    }

    /// The known scopes, in order.
    pub fn iter(&self) -> impl Iterator<Item = Scope> + '_ {
        self.known.iter().copied()
    }

    /// The scopes this crate does not know, as Asana wrote them.
    pub fn unknown(&self) -> impl Iterator<Item = &str> + '_ {
        self.unknown.iter().map(AsRef::as_ref)
    }
}

/// The OpenID Connect scopes, which govern ID tokens and the user info endpoint, not the API.
fn is_identity(scope: Scope) -> bool {
    matches!(scope, Scope::Openid | Scope::Email | Scope::Profile)
}

impl fmt::Display for ScopeSet {
    /// Asana's space-separated form: known scopes in order, then unknown ones.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self.known.iter().map(|scope| scope.as_str());
        let names = names.chain(self.unknown.iter().map(AsRef::as_ref));
        for (index, name) in names.enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            f.write_str(name)?;
        }
        Ok(())
    }
}

impl FromStr for ScopeSet {
    type Err = Infallible;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(text))
    }
}

impl FromIterator<Scope> for ScopeSet {
    fn from_iter<I: IntoIterator<Item = Scope>>(scopes: I) -> Self {
        Self {
            known: scopes.into_iter().collect(),
            unknown: BTreeSet::new(),
        }
    }
}

impl Extend<Scope> for ScopeSet {
    fn extend<I: IntoIterator<Item = Scope>>(&mut self, scopes: I) {
        self.known.extend(scopes);
    }
}

impl<const N: usize> From<[Scope; N]> for ScopeSet {
    fn from(scopes: [Scope; N]) -> Self {
        scopes.into_iter().collect()
    }
}

/// The scopes to request for the operations an app means to call:
/// `scopes_for![GetTask, UpdateTask]` is
/// `ScopeSet::new().with_endpoint::<GetTask>().with_endpoint::<UpdateTask>()`.
///
/// When any of them has no granular scope the answer is `default` (Full permissions); see
/// [`ScopeSet::with_endpoint`].
#[macro_export]
macro_rules! scopes_for {
    ($($endpoint:ty),* $(,)?) => {
        $crate::oauth::ScopeSet::new()$(.with_endpoint::<$endpoint>())*
    };
}
