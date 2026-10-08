//! Stable JSON error bodies for the API.
//!
//! Every error response is `{"ok": false, "error": <prose>, "kind": <kind>}`
//! with the stable `kind` strings of [`crate::Error::kind`], so a client
//! can branch without parsing prose.

use crate::Error;
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// An [`Error`] crossing the HTTP boundary.
#[derive(Debug)]
pub struct ApiError(pub Error);

pub type ApiResult<T> = Result<T, ApiError>;

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        Self(e)
    }
}

/// Map an error kind onto a status code. Infrastructure failures are 500;
/// everything a caller can fix stays in the 4xx range.
fn status_for(kind: &str) -> StatusCode {
    match kind {
        "validation" => StatusCode::BAD_REQUEST,
        "not_found" => StatusCode::NOT_FOUND,
        "permission" => StatusCode::FORBIDDEN,
        "budget" | "cancelled" => StatusCode::CONFLICT,
        "assessment_rejected" => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The stable JSON body for an error. Infrastructure failures (the 5xx
/// family) are logged in full but reported with generic prose: their
/// messages can carry file paths, SQL state, or provider detail that does
/// not belong on the wire. Caller mistakes keep their exact message.
pub(super) fn error_body(e: &crate::Error) -> serde_json::Value {
    let kind = e.kind();
    let message = if status_for(kind).is_server_error() {
        tracing::error!(error = %e, kind, "API request failed");
        format!("internal {kind} error; the server log has the detail")
    } else {
        e.to_string()
    };
    serde_json::json!({"ok": false, "error": message, "kind": kind})
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = status_for(self.0.kind());
        (status, Json(error_body(&self.0))).into_response()
    }
}

/// A JSON body rejection (malformed JSON, wrong content type, body over the
/// cap) is a caller mistake: report it with the rejection's own status and
/// the `validation` kind.
pub fn rejection_to_error(rejection: axum::extract::rejection::JsonRejection) -> Response {
    let body = serde_json::json!({
        "ok": false,
        "error": rejection.body_text(),
        "kind": "validation",
    });
    (rejection.status(), Json(body)).into_response()
}

/// Response mapper that rewrites error responses produced below the
/// handlers — the request timeout's empty 408, axum's empty 405 — into the
/// stable JSON error body, so "every error is `{ok, error, kind}`" holds
/// for the whole surface, not only for handler-produced errors.
///
/// Responses that already carry JSON (every handler error) pass through
/// untouched, as does anything that is not an error.
pub async fn ensure_json_error(response: Response) -> Response {
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let already_json = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if already_json {
        return response;
    }
    let kind = if status == StatusCode::REQUEST_TIMEOUT {
        "timeout"
    } else if status.is_server_error() {
        "internal"
    } else {
        "validation"
    };
    let error = status
        .canonical_reason()
        .unwrap_or("request failed")
        .to_ascii_lowercase();
    let body = serde_json::json!({"ok": false, "error": error, "kind": kind});
    (status, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    #[test]
    fn caller_mistakes_map_to_4xx_and_infrastructure_to_500() {
        assert_eq!(status_for("validation"), StatusCode::BAD_REQUEST);
        assert_eq!(status_for("not_found"), StatusCode::NOT_FOUND);
        assert_eq!(status_for("permission"), StatusCode::FORBIDDEN);
        assert_eq!(status_for("budget"), StatusCode::CONFLICT);
        assert_eq!(status_for("storage"), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(status_for("provider"), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn infrastructure_error_prose_is_redacted_from_the_wire() {
        let body = error_body(&Error::Storage(sqlx::Error::PoolTimedOut));
        assert_eq!(body["kind"], "storage");
        let message = body["error"].as_str().expect("error prose");
        assert!(
            !message.contains("pool"),
            "internal detail must not leak: {message}"
        );
        assert!(
            message.contains("server log"),
            "points at the log: {message}"
        );
    }

    #[test]
    fn caller_mistake_prose_is_kept() {
        // 4xx prose is the error's own kind-prefixed Display text.
        let body = error_body(&Error::validation("goal must not be empty"));
        assert_eq!(body["kind"], "validation");
        let message = body["error"].as_str().expect("error prose");
        assert!(
            message.contains("goal must not be empty"),
            "caller prose survives: {message}"
        );
    }
}
