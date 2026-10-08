//! Anonymous browser identity for the web API.
//!
//! One persistent, HttpOnly cookie (`uruk_browser`) holds an opaque
//! 256-bit bearer token that identifies a browser — no accounts, no
//! login, deliberately. The [`attach_identity`] middleware guarantees
//! every identity-scoped request carries an [`OwnerDigest`]; the
//! [`Owner`] and [`OwnedRun`] extractors are the only way handlers reach
//! it, so authorization cannot be skipped by forgetting a check inside a
//! handler body.
//!
//! Lifecycle of the token:
//!
//! - **Mint**: the first identity-scoped request without a valid cookie
//!   gets a fresh token from the OS CSPRNG, uses it for that same
//!   request, and receives it in a persistent `Set-Cookie`.
//! - **Carry**: the browser sends it back; only its SHA-256 digest
//!   ([`OwnerDigest`]) travels further into the process. The raw token is
//!   never stored and never logged.
//! - **Replace**: a malformed cookie (wrong length, wrong alphabet, or an
//!   oversized header) is treated as absent and silently replaced.
//! - **Lose**: clearing the cookie loses access to the browser's runs.
//!   There is no recovery path; that is the documented contract.

use super::AppState;
use super::error::ApiError;
use crate::Error;
use crate::records::RunId;
use crate::store::OwnerDigest;
use axum::RequestPartsExt;
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use rand::RngCore;

/// Name of the identity cookie. Host-only on purpose: no `Domain`
/// attribute is ever set, so the cookie never leaks to sibling hosts.
pub const COOKIE_NAME: &str = "uruk_browser";

/// Token length in hex characters: 32 CSPRNG bytes, 256 bits of entropy.
const TOKEN_HEX_LEN: usize = 64;

/// Cookie lifetime: one year, refreshed whenever a new token is minted.
const COOKIE_MAX_AGE_SECS: u64 = 31_536_000;

/// Upper bound on a `Cookie` header this module will scan. Anything
/// larger than what real browsers send (~4 KiB per cookie) is ignored
/// wholesale rather than parsed.
const MAX_COOKIE_HEADER_BYTES: usize = 8 * 1024;

/// Upper bound on a run id taken from the request path before it is used
/// in a query or echoed into an error message.
const MAX_RUN_ID_CHARS: usize = 128;

/// Mint a fresh opaque token: 256 bits from the OS CSPRNG, lowercase hex.
fn mint_token() -> String {
    let mut bytes = [0u8; TOKEN_HEX_LEN / 2];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// A token is exactly 64 lowercase hex characters; everything else is
/// treated as absent and replaced.
fn is_valid_token(value: &str) -> bool {
    value.len() == TOKEN_HEX_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Extract the first valid identity token from the request's `Cookie`
/// header(s). Bounded: oversized or non-ASCII headers are skipped, and
/// malformed values never fail the request — they are simply not a token.
fn token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter(|value| value.len() <= MAX_COOKIE_HEADER_BYTES)
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().strip_prefix(COOKIE_NAME))
        .filter_map(|rest| rest.strip_prefix('='))
        .find(|candidate| is_valid_token(candidate))
}

/// Build the `Set-Cookie` value for a freshly minted token.
///
/// Host-only (no `Domain`), `HttpOnly` (never readable from frontend
/// JavaScript), `Path=/`, `SameSite=Lax`, a one-year `Max-Age`, and
/// `Secure` when [`super::WebConfig::cookie_secure`] says the public
/// origin is HTTPS.
fn set_cookie_value(token: &str, secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; \
         Max-Age={COOKIE_MAX_AGE_SECS}{secure_attr}"
    )
}

/// Middleware for every identity-scoped route: resolve the browser's
/// identity (minting a token when there is none), stash the digest in the
/// request extensions for the [`Owner`]/[`OwnedRun`] extractors, and set
/// the cookie on the response when one was minted.
///
/// Health and the unknown-route fallback are mounted outside this
/// middleware, so they never mint cookies.
pub async fn attach_identity(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let (digest, minted) = match token_from_headers(request.headers()) {
        Some(token) => (OwnerDigest::from_token(token), None),
        None => {
            let token = mint_token();
            (OwnerDigest::from_token(&token), Some(token))
        }
    };
    request.extensions_mut().insert(digest);

    let mut response = next.run(request).await;

    if let Some(token) = minted {
        let cookie = set_cookie_value(&token, state.config().cookie_secure);
        match HeaderValue::from_str(&cookie) {
            Ok(value) => {
                response.headers_mut().append(header::SET_COOKIE, value);
            }
            // The value is hex plus fixed ASCII attributes; this arm would
            // mean the constants above were broken in a refactor.
            Err(_) => unreachable!("identity cookie is always a valid header value"),
        }
    }
    response
}

/// The requesting browser's identity, for handlers that are identity- but
/// not run-scoped (listing runs, starting a run, the library).
///
/// # Errors
///
/// Rejects with `permission` (403, fail closed) when the route was
/// mounted outside [`attach_identity`] — a wiring bug, not a caller
/// mistake, so it is also logged as an error.
pub struct Owner(pub OwnerDigest);

impl<S: Send + Sync> FromRequestParts<S> for Owner {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        match parts.extensions.get::<OwnerDigest>() {
            Some(digest) => Ok(Self(digest.clone())),
            None => {
                tracing::error!(
                    path = %parts.uri.path(),
                    "identity-scoped handler mounted outside the identity middleware"
                );
                Err(ApiError(Error::permission(
                    "this route is unavailable without a browser identity",
                )))
            }
        }
    }
}

/// A run id from the path, **already authorized** against the requesting
/// browser's identity. Run-scoped handlers take this instead of
/// `Path<String>`, which keeps the ownership check in exactly one place.
///
/// # Errors
///
/// Rejects with the same non-disclosing `not_found` for a run that does
/// not exist, a run owned by another browser, and an ownerless run.
pub struct OwnedRun(pub RunId);

impl FromRequestParts<AppState> for OwnedRun {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let Owner(owner) = parts.extract_with_state::<Owner, _>(state).await?;
        let Path(raw) = parts
            .extract::<Path<String>>()
            .await
            .map_err(|rejection| ApiError(Error::validation(rejection.body_text())))?;
        if raw.chars().count() > MAX_RUN_ID_CHARS {
            // Bounded echo: an absurd path segment is not reflected back.
            return Err(ApiError(Error::not_found("no such run")));
        }
        let run_id = RunId::from_raw(raw);
        state.store().get_run_owned(&run_id, &owner).await?;
        Ok(Self(run_id))
    }
}

/// Parse the `URUK_COOKIE_SECURE` setting: whether the identity cookie is
/// marked `Secure` (sent by browsers over HTTPS only).
///
/// Unset means `false`, which matches the documented local-first setup —
/// a loopback API reached over plain HTTP, where a `Secure` cookie would
/// not be reliably stored. Any deployment that serves the console through
/// an HTTPS reverse proxy must set it to `true`; `docs/WEB.md` spells
/// this out. The value is validated strictly so a typo fails the server
/// start instead of silently weakening the cookie.
///
/// # Errors
///
/// Returns a `validation` error for anything but `true`/`1`/`false`/`0`
/// (case-insensitive, surrounding whitespace ignored).
pub fn cookie_secure_from_env(value: Option<&str>) -> crate::Result<bool> {
    match value {
        None => Ok(false),
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            other => Err(Error::validation(format!(
                "URUK_COOKIE_SECURE must be \"true\"/\"1\" or \"false\"/\"0\", got {other:?}"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod mint_token {
        use super::*;

        #[test]
        fn produces_64_lowercase_hex_chars() {
            let token = mint_token();
            assert!(is_valid_token(&token), "minted token is valid: {token}");
        }

        #[test]
        fn produces_distinct_tokens() {
            assert_ne!(mint_token(), mint_token());
        }
    }

    mod is_valid_token {
        use super::*;

        #[test]
        fn accepts_exactly_64_lowercase_hex() {
            assert!(is_valid_token(&"a0".repeat(32)));
        }

        #[test]
        fn rejects_wrong_length_uppercase_and_non_hex() {
            for bad in ["", "abc", &"A".repeat(64), &"g".repeat(64), &"a".repeat(65)] {
                assert!(!is_valid_token(bad), "must reject {bad:?}");
            }
        }
    }

    mod token_from_headers {
        use super::*;

        fn headers(value: &str) -> HeaderMap {
            let mut map = HeaderMap::new();
            map.insert(header::COOKIE, HeaderValue::from_str(value).expect("ascii"));
            map
        }

        #[test]
        fn finds_the_token_among_other_cookies() {
            let token = "a".repeat(64);
            let map = headers(&format!("theme=dark; uruk_browser={token}; x=1"));
            assert_eq!(token_from_headers(&map), Some(token.as_str()));
        }

        #[test]
        fn ignores_an_oversized_header() {
            let token = "a".repeat(64);
            let map = headers(&format!(
                "junk={}; uruk_browser={token}",
                "x".repeat(MAX_COOKIE_HEADER_BYTES)
            ));
            assert_eq!(token_from_headers(&map), None);
        }

        #[test]
        fn ignores_a_malformed_value() {
            assert_eq!(token_from_headers(&headers("uruk_browser=nonsense")), None);
        }

        #[test]
        fn ignores_a_prefix_name_match() {
            let token = "a".repeat(64);
            assert_eq!(
                token_from_headers(&headers(&format!("uruk_browser2={token}"))),
                None,
                "uruk_browser2 is a different cookie"
            );
        }
    }

    mod set_cookie_value {
        use super::*;

        #[test]
        fn carries_the_persistence_and_scoping_attributes() {
            let cookie = set_cookie_value("deadbeef", false);
            assert_eq!(
                cookie,
                "uruk_browser=deadbeef; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000"
            );
        }

        #[test]
        fn appends_secure_when_configured() {
            assert!(set_cookie_value("deadbeef", true).ends_with("; Secure"));
        }
    }

    mod cookie_secure_from_env {
        use super::*;

        #[test]
        fn unset_defaults_to_not_secure() {
            assert!(!cookie_secure_from_env(None).expect("valid"));
        }

        #[test]
        fn accepts_the_documented_spellings() {
            for (raw, expected) in [
                ("true", true),
                ("1", true),
                ("FALSE", false),
                (" 0 ", false),
            ] {
                assert_eq!(
                    cookie_secure_from_env(Some(raw)).expect("valid"),
                    expected,
                    "{raw:?}"
                );
            }
        }

        #[test]
        fn rejects_anything_else() {
            let err = cookie_secure_from_env(Some("yes")).expect_err("invalid");
            assert!(
                err.to_string().contains("URUK_COOKIE_SECURE"),
                "names the setting: {err}"
            );
        }
    }
}
