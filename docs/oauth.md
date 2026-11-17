# OAuth

`asana_client::oauth` covers Asana's
[authorisation code grant](https://developers.asana.com/docs/oauth) with PKCE, and keeping a
client's access token fresh. It stores nothing itself: you implement `TokenStore` over your own
storage. Code blocks are taken from
[`crates/asana-client/examples/oauth_flow.rs`](../crates/asana-client/examples/oauth_flow.rs) and
from doc tests.

## The flow

1. Send the person to `OAuthApp::authorize_url` with a fresh `State`, a fresh `PkcePair`'s
   challenge and the scopes the app needs. Keep the state and the verifier with their session.
2. Asana redirects them back. `read_redirect` checks the `state` first, then any error, and
   returns the code.
3. `OAuthApp::exchange` trades the code and the verifier for an access token (an hour), a refresh
   token, and who the person is. Store them (`TokenResponse::into_stored`).
4. Build the client with `Auth::oauth(RefreshingTokenSource::new(app, store))`.
5. `OAuthApp::revoke` disconnects them.

```rust
let redirect_uri =
    env::var("ASANA_REDIRECT_URI").unwrap_or_else(|_| OUT_OF_BAND_REDIRECT_URI.to_owned());
let app = OAuthApp::builder()
    .client_id(env::var("ASANA_CLIENT_ID")?)
    .client_secret(env::var("ASANA_CLIENT_SECRET")?)
    .redirect_uri(redirect_uri)
    .build()?;

// 1. The scopes come from the operations the app will call, from the generated table. An
//    app registered with granular scopes must name them; see the OAuth guide.
let scopes = scopes_for![GetUser, GetTask, CreateStoryForTask];
// A fresh state and PKCE pair per attempt; a web app keeps both with the person's session.
let state = State::generate();
let pkce = PkcePair::generate();
println!(
    "Open this and approve the app:\n{}",
    app.authorize_url(&scopes, &state, pkce.challenge())
);

// 2. Read the answer: a redirect's query is checked against the state first.
println!("Paste the code, or the redirect's query string:");
let mut line = String::new();
std::io::stdin().lock().read_line(&mut line)?;
let line = line.trim();
let code = if line.contains("code=") {
    read_redirect(line, &state)?
} else {
    AuthorizationCode::new(line.to_owned())
};

// 3. Exchange it. The answer says who connected, and holds the refresh token.
let tokens = app.exchange(&code, pkce.verifier()).await?;
if let Some(user) = &tokens.user {
    println!("connected as {:?}", user.name);
}
// Store what `into_stored` returns in your own storage (a `TokenStore` over it); here, memory,
// in an `Arc` so this code can still read it after the client has its share.
let store = Arc::new(InMemoryTokenStore::new(
    tokens.into_stored(OffsetDateTime::now_utc())?,
));

// 4. The client refreshes the access token before it expires and after a 401, and saves the
//    new one through the store. `AuthError::Revoked` means the person must connect again.
let client = Client::builder()
    .auth(Auth::oauth(RefreshingTokenSource::new(
        app.clone(),
        Arc::clone(&store),
    )))
    .build()?;
let me = client.users().me().send().await?;
println!("the API sees {:?}", me.email);

// 5. Disconnect: revoke the refresh token (and delete it from storage).
if let Some(stored) = store.get() {
    app.revoke(&stored.refresh_token).await?;
}
```

The token calls go to `https://app.asana.com/-/…`, outside the API, and authenticate with the
client secret, so they do not go through `Client`. They do go through a `Transport`: pass the
same one the client uses to `OAuthApp::builder().transport(..)` so your TLS and proxy settings
apply, or a test transport so tests need no network. None of them is retried, because an
authorisation code works once.

## Redirect URIs

The redirect URI must be exactly what the app registered, and is never normalised. Asana accepts
an `https` URL, an `http` URL on `localhost`, `127.0.0.1` or `[::1]`, or the out-of-band
`urn:ietf:wg:oauth:2.0:oob` (`OUT_OF_BAND_REDIRECT_URI`), with which Asana shows the code for the
person to paste. `OAuthApp::builder().build()` refuses anything else.

PKCE is always used, with Asana's one method, `S256`. Asana requires it for localhost and
out-of-band redirects, and it costs nothing elsewhere.

## Scopes

Each generated descriptor names the granular scope its operation needs in `Endpoint::SCOPE`, and
`scopes_for![..]` or `ScopeSet::with_endpoint::<E>()` collects them. From the `oauth::scopes`
doc test:

```rust
use asana_client::oauth::ScopeSet;
use asana_client::openapi::endpoints::{GetTask, UpdateTask};
use asana_client::openapi::scopes::Scope;

let wanted = asana_client::scopes_for![GetTask, UpdateTask];
assert!(wanted.contains(Scope::TasksRead) && wanted.contains(Scope::TasksWrite));

let granted = ScopeSet::parse("tasks:read projects:read");
assert_eq!(granted.missing(&wanted).to_string(), "tasks:write");
```

Asana's rules, as its documentation states them:

- An app is registered either for **granular scopes** or for **Full permissions** (the `default`
  scope), and cannot ask for both. An app with granular scopes must name them; an empty set
  sends no `scope`, which means `default`.
- Operations with no granular scope yet are reached only with Full permissions. When
  `with_endpoint` meets one, the set becomes `default` in place of the granular API scopes, keeping
  `openid`, `email` and `profile`. [coverage.md](coverage.md) shows which operations have a
  granular scope.
- Scopes do not imply each other: `tasks:write` does not grant `tasks:read`.

`ScopeSet` keeps scopes it does not know as text, so a set read from Asana (`TokenInfo::scope`
from `introspect`) writes back exactly what it read.

## Keeping tokens: `TokenStore` and `RefreshingTokenSource`

A `TokenStore` keeps one person's tokens. It has two methods: `load`, and `replace(expected,
tokens)`, which stores the new tokens only if the stored refresh token is still `expected`, in
one atomic step (`UPDATE … WHERE refresh_token = $expected`), and answers `Saved` or `Conflict`.
A service with many connected people has one store and one source per person. `Arc<T>` of a
store is a store.

`RefreshingTokenSource` is the `TokenSource` a client calls before every request:

- It answers from memory while the access token has more than a margin left (five minutes,
  `with_margin` to change it).
- Otherwise it refreshes under one async lock, so ten tasks that find the token stale at once
  cause one refresh. Before refreshing it reads the store, in case another process already has;
  after losing the compare-and-swap it takes the tokens the winner stored.
- On a `401` the client tells it the token was rejected; the next call refreshes, and the client
  retries the request once.
- A dead refresh token (`invalid_grant`) is `AuthError::Revoked`, which reaches your code as
  `Error::Auth(AuthError::Revoked)`: the person must connect again. Any other failure to refresh
  is `AuthError::Unavailable`, which may pass.

Asana documents a refresh token only in the answer to a code exchange, not as rotating on
refresh; when a refresh answer does carry one, it is stored.

## Revoking and introspecting

`revoke(&refresh_token)` takes only a refresh token, which is all Asana's revoke endpoint
accepts, and succeeds whether or not the token was still live. `introspect(&token)` asks
`POST /-/token_info` about any token (an OAuth access or refresh token, a personal access token
or a service account's) and returns a `TokenInfo`: whether it is active, its expiry, its scopes
and the client id.

## Errors

The token calls return `OAuthError`. `needs_reconnect()` says whether the grant is dead
(`invalid_grant`) and the person must authorise again; `is_retryable()` whether trying later
could work (a `5xx`, a `429`, a transient transport failure). A denied authorisation on the
redirect is `OAuthError::Denied`, and a redirect whose `state` does not match is
`OAuthError::StateMismatch`. Like every error in this workspace, its `Display` holds no token, code or secret.
`AuthorizationCode`, `RefreshToken`, `AccessToken`, `PkceVerifier` and the client secret are
`secrecy` types whose `Debug` is redacted.
