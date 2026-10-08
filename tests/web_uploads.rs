//! Owner-bound file uploads for the web API.
//!
//! The browser never supplies a server path: it posts bytes, the server
//! stores them under a server-managed directory keyed by the owner's
//! digest, and runs reference uploads by id. Uploads are bounded in size,
//! count, and type; one browser's uploads are invisible to every other
//! browser, and foreign ids answer exactly like unknown ids.
//!
//! Everything runs offline against temporary SQLite stores through
//! `tower::ServiceExt::oneshot`, with cookies carried by hand: each
//! distinct token below is an independent "browser".
#![cfg(feature = "web")]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use uruk::provider::MockProvider;
use uruk::web::{AppState, WebConfig, router};

/// A valid-format browser token: 64 lowercase hex characters (256 bits).
fn token(fill: char) -> String {
    std::iter::repeat_n(fill, 64).collect()
}

async fn app_with(config: WebConfig) -> (axum::Router, uruk::store::Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let state = AppState::new(store.clone(), Arc::new(MockProvider::new()), config);
    (router(state), store, dir)
}

async fn empty_app() -> (axum::Router, uruk::store::Store, tempfile::TempDir) {
    app_with(WebConfig::default()).await
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response body is JSON")
}

fn get(uri: &str, cookie_token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::COOKIE, format!("uruk_browser={cookie_token}"))
        .body(Body::empty())
        .expect("request")
}

fn post_bytes(uri: &str, bytes: Vec<u8>, cookie_token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::COOKIE, format!("uruk_browser={cookie_token}"))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(bytes))
        .expect("request")
}

fn delete(uri: &str, cookie_token: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(header::COOKIE, format!("uruk_browser={cookie_token}"))
        .body(Body::empty())
        .expect("request")
}

/// Upload `bytes` as `name` for `cookie_token` and return the upload JSON.
async fn upload(
    app: &axum::Router,
    name: &str,
    bytes: &[u8],
    cookie_token: &str,
) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(post_bytes(
            &format!("/api/uploads?name={name}"),
            bytes.to_vec(),
            cookie_token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    assert_eq!(body["ok"], true);
    body["upload"].clone()
}

/// Every file below the project's upload root, relative to it.
fn files_under_uploads(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let root = dir.join(".uruk/uploads");
    let mut found = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path.strip_prefix(&root).expect("under root").to_path_buf());
            }
        }
    }
    found
}

mod creating {
    use super::*;

    #[tokio::test]
    async fn an_upload_returns_metadata_and_never_a_server_path() {
        let (app, _store, _dir) = empty_app().await;
        let response = app
            .oneshot(post_bytes(
                "/api/uploads?name=drift-notes.txt",
                b"the measured drift notes".to_vec(),
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        assert_eq!(body["ok"], true);
        let upload = &body["upload"];
        assert!(
            upload["id"].as_str().expect("id").starts_with("upl_"),
            "server-minted id: {upload}"
        );
        assert_eq!(upload["file_name"], "drift-notes.txt");
        assert_eq!(upload["size_bytes"], 24);
        assert!(
            upload["content_hash"]
                .as_str()
                .is_some_and(|h| !h.is_empty()),
            "content hash recorded: {upload}"
        );
        // The server's storage layout is its own business: no path, no
        // directory name, nothing a client could feed back as a locator.
        let serialized = body.to_string();
        assert!(
            !serialized.contains("path") && !serialized.contains(".uruk"),
            "no server path crosses the wire: {serialized}"
        );
    }

    #[tokio::test]
    async fn the_stored_file_lives_under_the_owner_digest_never_the_token() {
        let (app, _store, dir) = empty_app().await;
        let raw_token = token('a');
        upload(&app, "notes.txt", b"contents", &raw_token).await;

        let files = files_under_uploads(dir.path());
        assert_eq!(files.len(), 1, "exactly one stored file: {files:?}");
        let digest = uruk::store::OwnerDigest::from_token(&raw_token);
        let stored = files[0].to_string_lossy().into_owned();
        assert!(
            stored.starts_with(digest.as_str()),
            "keyed by the one-way digest: {stored}"
        );
        assert!(
            !stored.contains(&raw_token),
            "the raw token never reaches disk: {stored}"
        );
        assert!(
            !stored.contains("notes"),
            "the client-chosen name never becomes a disk path: {stored}"
        );
    }

    #[tokio::test]
    async fn a_missing_or_blank_name_is_a_validation_error() {
        let (app, _store, _dir) = empty_app().await;
        for uri in [
            "/api/uploads",
            "/api/uploads?name=",
            "/api/uploads?name=%20",
        ] {
            let response = app
                .clone()
                .oneshot(post_bytes(uri, b"x".to_vec(), &token('a')))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body = body_json(response).await;
            assert_eq!(body["kind"], "validation", "{uri}");
        }
    }

    #[tokio::test]
    async fn path_like_names_are_rejected_and_nothing_is_stored() {
        let (app, _store, dir) = empty_app().await;
        // %2F is '/', %5C is '\', %2E%2E is '..': every spelling of "this
        // name navigates the filesystem" must die at validation.
        for name in [
            "dir%2Fevil.txt",
            "%2Fetc%2Fpasswd",
            "..%2Fsecret.txt",
            "evil%5Cname.txt",
            "%2E%2E",
        ] {
            let response = app
                .clone()
                .oneshot(post_bytes(
                    &format!("/api/uploads?name={name}"),
                    b"x".to_vec(),
                    &token('a'),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
            let body = body_json(response).await;
            assert_eq!(body["kind"], "validation", "{name}");
        }
        assert!(
            files_under_uploads(dir.path()).is_empty(),
            "a rejected upload stores nothing"
        );
    }

    #[tokio::test]
    async fn a_disallowed_file_type_is_rejected_with_the_allowed_list() {
        let (app, _store, _dir) = empty_app().await;
        for name in ["payload.exe", "archive.zip", "noextension", ".txt"] {
            let response = app
                .clone()
                .oneshot(post_bytes(
                    &format!("/api/uploads?name={name}"),
                    b"x".to_vec(),
                    &token('a'),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
            let body = body_json(response).await;
            assert_eq!(body["kind"], "validation", "{name}");
            assert!(
                body["error"].as_str().expect("prose").contains("pdf"),
                "names the allowed types: {body}"
            );
        }
    }

    #[tokio::test]
    async fn an_empty_body_is_a_validation_error() {
        let (app, _store, _dir) = empty_app().await;
        let response = app
            .oneshot(post_bytes(
                "/api/uploads?name=empty.txt",
                vec![],
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["kind"], "validation");
    }

    #[tokio::test]
    async fn an_upload_over_the_size_cap_keeps_the_stable_error_body() {
        let (app, _store, _dir) = app_with(WebConfig {
            max_upload_bytes: 1024,
            ..WebConfig::default()
        })
        .await;
        let response = app
            .oneshot(post_bytes(
                "/api/uploads?name=big.txt",
                vec![b'x'; 4 * 1024],
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = body_json(response).await;
        assert_eq!(body["ok"], false);
        assert_eq!(body["kind"], "validation");
    }

    #[tokio::test]
    async fn uploads_larger_than_the_command_body_cap_still_fit_the_upload_cap() {
        // Commands are capped below this size; uploads deliberately are not.
        let (app, _store, _dir) = empty_app().await;
        let response = app
            .oneshot(post_bytes(
                "/api/uploads?name=big.txt",
                vec![b'x'; 256 * 1024],
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn the_per_owner_count_cap_binds_that_owner_only() {
        let (app, _store, _dir) = app_with(WebConfig {
            max_uploads_per_owner: 2,
            ..WebConfig::default()
        })
        .await;
        upload(&app, "one.txt", b"1", &token('a')).await;
        upload(&app, "two.txt", b"2", &token('a')).await;

        let response = app
            .clone()
            .oneshot(post_bytes(
                "/api/uploads?name=three.txt",
                b"3".to_vec(),
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["kind"], "validation");

        // A different browser is not consuming this owner's allowance.
        upload(&app, "theirs.txt", b"4", &token('b')).await;
    }
}

mod isolation {
    use super::*;

    #[tokio::test]
    async fn the_listing_shows_only_the_owners_uploads() {
        let (app, _store, _dir) = empty_app().await;
        upload(&app, "mine-1.txt", b"a", &token('a')).await;
        upload(&app, "mine-2.txt", b"b", &token('a')).await;
        upload(&app, "theirs.txt", b"c", &token('b')).await;

        let response = app
            .clone()
            .oneshot(get("/api/uploads", &token('a')))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let uploads = body["uploads"].as_array().expect("uploads");
        assert_eq!(uploads.len(), 2);
        for entry in uploads {
            assert!(
                entry["file_name"]
                    .as_str()
                    .expect("file_name")
                    .starts_with("mine-"),
                "only the owner's uploads: {entry}"
            );
        }

        let response = app.oneshot(get("/api/uploads", &token('b'))).await.unwrap();
        let body = body_json(response).await;
        assert_eq!(body["uploads"].as_array().expect("uploads").len(), 1);
    }

    #[tokio::test]
    async fn foreign_and_unknown_upload_deletes_are_indistinguishable() {
        let (app, _store, _dir) = empty_app().await;
        let theirs = upload(&app, "theirs.txt", b"owned by a", &token('a')).await;
        let theirs_id = theirs["id"].as_str().expect("id");

        let foreign = app
            .clone()
            .oneshot(delete(&format!("/api/uploads/{theirs_id}"), &token('b')))
            .await
            .unwrap();
        let unknown = app
            .clone()
            .oneshot(delete("/api/uploads/upl_does_not_exist", &token('b')))
            .await
            .unwrap();
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let foreign_body = body_json(foreign).await;
        let unknown_body = body_json(unknown).await;
        assert_eq!(foreign_body["kind"], "not_found");
        assert_eq!(
            foreign_body["error"], unknown_body["error"],
            "probing discloses nothing"
        );

        // The failed foreign delete removed nothing.
        let response = app.oneshot(get("/api/uploads", &token('a'))).await.unwrap();
        let body = body_json(response).await;
        assert_eq!(body["uploads"].as_array().expect("uploads").len(), 1);
    }

    #[tokio::test]
    async fn the_owner_can_delete_their_upload_and_its_bytes_leave_the_disk() {
        let (app, _store, dir) = empty_app().await;
        let mine = upload(&app, "mine.txt", b"delete me", &token('a')).await;
        let mine_id = mine["id"].as_str().expect("id");
        assert_eq!(files_under_uploads(dir.path()).len(), 1);

        let response = app
            .clone()
            .oneshot(delete(&format!("/api/uploads/{mine_id}"), &token('a')))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["ok"], true);

        let response = app.oneshot(get("/api/uploads", &token('a'))).await.unwrap();
        let body = body_json(response).await;
        assert_eq!(body["uploads"], serde_json::json!([]));
        assert!(
            files_under_uploads(dir.path()).is_empty(),
            "deleted bytes do not linger on disk"
        );
    }

    #[tokio::test]
    async fn a_fresh_browser_has_an_empty_upload_list() {
        let (app, _store, _dir) = empty_app().await;
        let response = app.oneshot(get("/api/uploads", &token('f'))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["uploads"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn uploads_survive_a_server_restart_for_the_same_browser() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join(".uruk/state.sqlite");

        {
            let store = uruk::store::Store::open(&db).await.expect("open store");
            let state = AppState::new(
                store.clone(),
                Arc::new(MockProvider::new()),
                WebConfig::default(),
            );
            let app = router(state);
            upload(&app, "durable.txt", b"still here", &token('a')).await;
            store.close().await;
        }

        let store = uruk::store::Store::open(&db).await.expect("reopen store");
        let app = router(AppState::new(
            store,
            Arc::new(MockProvider::new()),
            WebConfig::default(),
        ));
        let response = app.oneshot(get("/api/uploads", &token('a'))).await.unwrap();
        let body = body_json(response).await;
        let uploads = body["uploads"].as_array().expect("uploads");
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0]["file_name"], "durable.txt");
    }
}
