//! The OAuth authorisation code flow with PKCE, from the command line, then a client whose
//! access token refreshes itself.
//!
//! ```text
//! ASANA_CLIENT_ID=...       the app's client id, from Asana's developer console
//! ASANA_CLIENT_SECRET=...   its client secret
//! ASANA_REDIRECT_URI=...    a redirect URI registered for the app; default the out-of-band one
//! cargo run -p asana-client --example oauth_flow
//! ```
//!
//! Open the printed URL, approve the app, and paste back either the code Asana shows (out of
//! band) or the whole query string of the redirect (`code=...&state=...`).

use std::env;
use std::io::BufRead;
use std::sync::Arc;

use asana_client::oauth::{
    AuthorizationCode, InMemoryTokenStore, OAuthApp, OUT_OF_BAND_REDIRECT_URI, PkcePair,
    RefreshingTokenSource, State, read_redirect,
};
use asana_client::openapi::endpoints::{CreateStoryForTask, GetTask, GetUser};
use asana_client::{Auth, Client, scopes_for};
use time::OffsetDateTime;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    Ok(())
}
