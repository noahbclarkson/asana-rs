//! The authorisation URL, PKCE, the state, the redirect back, and what the app builder accepts.

use asana_client::oauth::{
    OAuthApp, OAuthBuildError, OAuthError, OAuthErrorCode, PkceChallenge, PkcePair, PkceVerifier,
    ScopeSet, State, read_redirect,
};
use asana_client::openapi::scopes::Scope;

use crate::fixtures::{CLIENT_ID, CLIENT_SECRET, REDIRECT, app};
use crate::support::{Scripted, reply};

/// RFC 7636, appendix B.
const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const RFC_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn quiet() -> std::sync::Arc<Scripted> {
    Scripted::new(|_, _| Ok(reply(500, "")))
}

#[test]
fn the_pkce_challenge_is_rfc_7636s_known_answer() {
    let pair = PkcePair::from_verifier(PkceVerifier::new(RFC_VERIFIER).unwrap());
    assert_eq!(pair.challenge().as_str(), RFC_CHALLENGE);
    assert_eq!(PkceChallenge::METHOD, "S256");
}

#[test]
fn a_generated_verifier_is_43_unreserved_characters_and_never_repeats() {
    let first = PkcePair::generate();
    let second = PkcePair::generate();
    let text = |pair: &PkcePair| {
        asana_client::ExposeSecret::expose_secret(pair.verifier().secret()).to_owned()
    };
    assert_eq!(text(&first).len(), 43);
    assert_ne!(text(&first), text(&second));
    // It is a valid verifier, and its challenge is the one computed from it.
    let again = PkcePair::from_verifier(PkceVerifier::new(text(&first)).unwrap());
    assert_eq!(again.challenge(), first.challenge());
}

#[test]
fn a_verifier_outside_rfc_7636s_length_or_alphabet_is_refused() {
    assert!(PkceVerifier::new("a".repeat(42)).is_err());
    assert!(PkceVerifier::new("a".repeat(129)).is_err());
    assert!(PkceVerifier::new(format!("{}+", "a".repeat(43))).is_err());
    assert!(PkceVerifier::new("a".repeat(128)).is_ok());
    assert!(PkceVerifier::new(format!("{}-._~", "a".repeat(40))).is_ok());
}

#[test]
fn the_authorize_url_carries_exactly_asanas_parameters_with_spaces_as_percent_20() {
    let app = app(quiet());
    let pkce = PkcePair::from_verifier(PkceVerifier::new(RFC_VERIFIER).unwrap());
    let scopes = ScopeSet::from([Scope::TasksRead, Scope::ProjectsRead]);
    let url = app.authorize_url(&scopes, &State::new("acme-state"), pkce.challenge());
    assert_eq!(
        url,
        "https://app.asana.com/-/oauth_authorize\
         ?client_id=1201234567890123\
         &redirect_uri=https%3A%2F%2Facme.example.com%2Fasana%2Fcallback\
         &response_type=code\
         &state=acme-state\
         &code_challenge_method=S256\
         &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM\
         &scope=projects%3Aread%20tasks%3Aread"
    );
}

#[test]
fn an_empty_scope_set_leaves_the_scope_parameter_out() {
    let url = app(quiet()).authorize_url(
        &ScopeSet::new(),
        &State::new("s"),
        PkcePair::generate().challenge(),
    );
    assert!(!url.contains("scope="), "{url}");
}

#[test]
fn a_state_matches_only_itself() {
    let state = State::generate();
    assert!(state.matches(state.as_str()));
    assert!(!state.matches(&state.as_str()[1..]));
    assert!(!state.matches(""));
    assert!(!State::generate().matches(state.as_str()));
    assert_eq!(state.as_str().len(), 43);
}

#[test]
fn the_redirect_gives_the_code_only_when_its_state_matches() {
    let state = State::new("acme-state");
    let code = read_redirect("?code=325797325&state=acme-state", &state).unwrap();
    assert_eq!(
        asana_client::ExposeSecret::expose_secret(code.secret()),
        "325797325"
    );

    let forged = read_redirect("code=325797325&state=other", &state).unwrap_err();
    assert!(matches!(forged, OAuthError::StateMismatch), "{forged:?}");
    let no_state = read_redirect("code=325797325", &state).unwrap_err();
    assert!(matches!(no_state, OAuthError::StateMismatch));
    // A forged error is refused for its state too, before its error is believed.
    let forged_error = read_redirect("error=access_denied&state=other", &state).unwrap_err();
    assert!(matches!(forged_error, OAuthError::StateMismatch));
}

#[test]
fn a_declined_or_forbidden_authorisation_comes_back_as_denied() {
    let state = State::new("acme-state");
    let denied = read_redirect("error=access_denied&state=acme-state", &state).unwrap_err();
    assert!(matches!(
        denied,
        OAuthError::Denied {
            code: OAuthErrorCode::AccessDenied,
            ..
        }
    ));
    let forbidden = read_redirect(
        "error=forbidden_scopes&error_description=Your+app+is+not+allowed&state=acme-state",
        &state,
    )
    .unwrap_err();
    let OAuthError::Denied { code, description } = forbidden else {
        panic!("{forbidden:?}");
    };
    assert_eq!(code, OAuthErrorCode::ForbiddenScopes);
    assert_eq!(description.as_deref(), Some("Your app is not allowed"));
    let empty = read_redirect("state=acme-state", &state).unwrap_err();
    assert!(matches!(empty, OAuthError::MissingCode));
}

fn build(redirect: &str) -> Result<OAuthApp, OAuthBuildError> {
    OAuthApp::builder()
        .client_id(CLIENT_ID)
        .client_secret(CLIENT_SECRET)
        .redirect_uri(redirect)
        .transport(quiet())
        .build()
}

#[test]
fn the_builder_accepts_only_redirects_asana_accepts_and_keeps_them_as_given() {
    for good in [
        "https://acme.example.com",
        "https://acme.example.com/cb?x=1",
        "http://localhost:8080/cb",
        "http://127.0.0.1/cb",
        "urn:ietf:wg:oauth:2.0:oob",
    ] {
        assert_eq!(build(good).unwrap().redirect_uri(), good);
    }
    for bad in [
        "http://acme.example.com/cb",
        "https://acme.example.com/cb#frag",
        "ftp://acme.example.com",
        "/cb",
        "",
    ] {
        assert!(
            matches!(build(bad), Err(OAuthBuildError::InvalidRedirectUri)),
            "{bad}"
        );
    }
}

#[test]
fn the_builder_refuses_an_empty_client_id_and_an_origin_with_a_path() {
    let empty = OAuthApp::builder()
        .client_id(" ")
        .client_secret(CLIENT_SECRET)
        .redirect_uri(REDIRECT)
        .transport(quiet())
        .build();
    assert!(matches!(empty, Err(OAuthBuildError::EmptyClientId)));
    let pathed = OAuthApp::builder()
        .client_id(CLIENT_ID)
        .client_secret(CLIENT_SECRET)
        .redirect_uri(REDIRECT)
        .origin("https://asana.example.com/api")
        .transport(quiet())
        .build();
    assert!(matches!(pathed, Err(OAuthBuildError::InvalidOrigin)));
    let fake = OAuthApp::builder()
        .client_id(CLIENT_ID)
        .client_secret(CLIENT_SECRET)
        .redirect_uri(REDIRECT)
        .origin("http://127.0.0.1:4010/")
        .transport(quiet())
        .build()
        .unwrap();
    assert_eq!(fake.origin(), "http://127.0.0.1:4010");
}
