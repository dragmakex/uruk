//! Operator-only, bounded request diagnostics in the project's state SQLite.
//! Starts commit before dispatch. Normal completion commits before EOF/return;
//! dropped futures schedule a cancellation update, and crash leftovers remain
//! identifiable and are closed as interrupted on the next server startup.
use crate::{Result, store::Store};

#[cfg(test)]
mod tests;
use axum::http::HeaderMap;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) const BODY_LIMIT: usize = 1024 * 1024;

// Diagnostic writes participate in the same writer admission as domain writes.
// Bypassing it can race cancelled transactions with a new SQLite busy handler.
async fn write<'q>(
    store: &Store,
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
) -> Result<()> {
    let mut tx = store.begin_write().await?;
    query.execute(tx.conn()).await?;
    tx.commit().await
}

#[derive(Clone, Debug)]
pub(crate) struct LogContext {
    pub store: Store,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub logical_id: String,
}

impl LogContext {
    pub async fn record_retry_delay(&self, attempt: u32, delay: Duration) -> Result<()> {
        write(
            &self.store,
            sqlx::query(
                "UPDATE request_logs SET retry_delay_ms=? WHERE logical_id=? AND attempt=?",
            )
            .bind(i64::try_from(delay.as_millis()).unwrap_or(i64::MAX))
            .bind(&self.logical_id)
            .bind(i64::from(attempt)),
        )
        .await?;
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Meta {
    pub direction: &'static str,
    pub request_id: String,
    pub logical_id: Option<String>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub attempt: Option<u32>,
}

/// Only deliberately safe header values are persisted. Unknown headers may
/// carry application-specific credentials, so preserving their names is enough.
pub(crate) fn headers(headers: &HeaderMap) -> Value {
    let mut values = serde_json::Map::new();
    for name in headers.keys() {
        let key = name.as_str();
        let safe = matches!(
            key,
            "content-type"
                | "content-length"
                | "accept"
                | "host"
                | "user-agent"
                | "x-request-id"
                | "traceparent"
                | "retry-after"
                | "date"
                | "server"
        ) || key.starts_with("x-ratelimit-")
            || key.starts_with("ratelimit-");
        values.insert(
            key.into(),
            json!(
                headers
                    .get_all(name)
                    .iter()
                    .map(|v| if safe {
                        v.to_str().unwrap_or("[NON-UTF8]")
                    } else {
                        "[REDACTED]"
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }
    Value::Object(values)
}

fn sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('-', "_");
    key == "token"
        || key.ends_with("_token")
        || [
            "authorization",
            "cookie",
            "password",
            "passwd",
            "secret",
            "api_key",
            "apikey",
            "credential",
            "signature",
        ]
        .iter()
        .any(|word| key.contains(word))
}

#[derive(Clone, Default)]
pub(crate) struct Redactor(pub Vec<String>);
impl Redactor {
    pub fn observe_url(&mut self, raw: &str) {
        let absolute = if raw.starts_with('/') {
            format!("http://diagnostic.invalid{raw}")
        } else {
            raw.to_owned()
        };
        if let Ok(url) = reqwest::Url::parse(&absolute) {
            if !url.username().is_empty() {
                self.0.push(url.username().to_owned());
            }
            if let Some(password) = url.password() {
                self.0.push(password.to_owned());
            }
            self.0.extend(
                url.query_pairs()
                    .filter(|(key, _)| sensitive(key))
                    .map(|(_, value)| value.into_owned()),
            );
        }
    }
    pub fn observe_capture(&mut self, capture: &Capture) {
        fn collect(value: &Value, secrets: &mut Vec<String>, secret: bool) {
            match value {
                Value::Object(map) => {
                    for (key, value) in map {
                        collect(value, secrets, secret || sensitive(key));
                    }
                }
                Value::Array(items) => {
                    for value in items {
                        collect(value, secrets, secret);
                    }
                }
                Value::String(text) if secret => secrets.push(text.to_owned()),
                _ => {}
            }
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&capture.bytes) {
            collect(&value, &mut self.0, false);
        }
    }
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let mut secrets = Vec::new();
        for (key, value) in headers {
            if sensitive(key.as_str())
                && let Ok(value) = value.to_str()
            {
                secrets.push(value.to_owned());
                if let Some(token) = value.strip_prefix("Bearer ") {
                    secrets.push(token.to_owned());
                }
                if key == "cookie" {
                    secrets.extend(
                        value.split(';').filter_map(|part| {
                            part.trim().split_once('=').map(|(_, v)| v.to_owned())
                        }),
                    );
                }
            }
        }
        Self(secrets)
    }
    pub fn text(&self, text: &str) -> String {
        let mut output = text.to_owned();
        for secret in &self.0 {
            if !secret.is_empty() {
                output = output.replace(secret, "[REDACTED]");
            }
        }
        output
    }
    pub fn value(&self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if sensitive(key) {
                        *value = json!("[REDACTED]");
                    } else {
                        self.value(value);
                    }
                }
            }
            Value::Array(items) => {
                for value in items {
                    self.value(value);
                }
            }
            Value::String(text) => *text = self.text(text),
            _ => {}
        }
    }
}

pub(crate) fn url(raw: &str) -> String {
    let relative = raw.starts_with('/');
    let parsed = if relative {
        format!("http://diagnostic.invalid{raw}")
    } else {
        raw.to_owned()
    };
    let Ok(mut url) = reqwest::Url::parse(&parsed) else {
        return "[INVALID URL]".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    let pairs: Vec<_> = url
        .query_pairs()
        .map(|(k, v)| {
            let v = if sensitive(&k) {
                "[REDACTED]".into()
            } else {
                v.into_owned()
            };
            (k.into_owned(), v)
        })
        .collect();
    if url.query().is_some() {
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    if relative {
        format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        )
    } else {
        url.to_string()
    }
}

#[derive(Default, Clone)]
pub(crate) struct Capture {
    bytes: Vec<u8>,
    pub total: u64,
    pub complete: bool,
    pub error: Option<String>,
}
impl Capture {
    pub fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        let available = BODY_LIMIT.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(available)]);
    }
    pub fn snapshot(&self, content_type: &str, redactor: &Redactor) -> Value {
        let truncated = self.total > self.bytes.len() as u64
            || (!self.complete && self.bytes.len() >= BODY_LIMIT);
        let textual = content_type.contains("json")
            || content_type.starts_with("text/")
            || content_type.is_empty();
        let body = if !textual || std::str::from_utf8(&self.bytes).is_err() {
            json!({"omitted": "binary body"})
        } else if truncated {
            json!({"omitted": "body exceeds 1 MiB capture limit"})
        } else if let Ok(mut value) = serde_json::from_slice::<Value>(&self.bytes) {
            redactor.value(&mut value);
            value
        } else {
            let text = String::from_utf8_lossy(&self.bytes);
            // A broken JSON document cannot be safely key-redacted.
            if content_type.contains("json")
                && ["password", "secret", "token", "authorization", "api_key"]
                    .iter()
                    .any(|key| text.to_ascii_lowercase().contains(key))
            {
                json!({"omitted": "malformed JSON containing credential fields"})
            } else {
                json!(redactor.text(&text))
            }
        };
        json!({"body":body,"bytes":self.total,"captured_bytes":self.bytes.len(),"truncated":truncated,"complete":self.complete,"read_error":self.error.as_ref().map(|e|redactor.text(e))})
    }
}
pub(crate) type SharedCapture = Arc<Mutex<Capture>>;
pub(crate) fn capture_lock(capture: &SharedCapture) -> std::sync::MutexGuard<'_, Capture> {
    capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        parts.push(error.to_string());
        source = error.source();
    }
    parts.join("\ncaused by: ")
}

/// Owns one physical request, never a whole retry loop.
pub(crate) struct RequestLog {
    store: Store,
    pub id: String,
    started: Instant,
    pub request: Value,
    pub response: Value,
    pub status: Option<u16>,
    pub response_capture: Option<(Capture, String)>,
    pub redactor: Redactor,
    done: bool,
}
impl RequestLog {
    pub async fn begin(
        store: Store,
        meta: Meta,
        mut request: Value,
        redactor: Redactor,
    ) -> Result<Self> {
        redactor.value(&mut request);
        let id = uuid::Uuid::new_v4().to_string();
        let started = Instant::now();
        let timestamp = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .expect("timestamp");
        write(&store, sqlx::query("INSERT INTO request_logs(id,request_id,logical_id,direction,run_id,task_id,attempt,started_at,request) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind(&id).bind(redactor.text(&meta.request_id)).bind(meta.logical_id).bind(meta.direction).bind(meta.run_id.map(|id|redactor.text(&id))).bind(meta.task_id)
            .bind(meta.attempt.map(i64::from)).bind(timestamp).bind(request.to_string())).await?;
        Ok(Self {
            store,
            id,
            started,
            request,
            response: json!({}),
            status: None,
            response_capture: None,
            redactor,
            done: false,
        })
    }
    pub async fn checkpoint(&mut self) -> Result<()> {
        if let Some((capture, content_type)) = &self.response_capture {
            self.response["content"] = capture.snapshot(content_type, &self.redactor);
        }
        self.redactor.value(&mut self.request);
        self.redactor.value(&mut self.response);
        write(&self.store, sqlx::query("UPDATE request_logs SET status=?,request=?,response=?,run_id=COALESCE(run_id,?) WHERE id=?")
            .bind(self.status.map(i64::from))
            .bind(self.request.to_string())
            .bind(self.response.to_string())
            .bind(self.response.pointer("/content/body/run_id").and_then(Value::as_str))
            .bind(&self.id)
            ).await?;
        Ok(())
    }
    pub async fn finish(
        &mut self,
        outcome: &str,
        error: Option<&str>,
        retry_delay: Option<Duration>,
    ) -> Result<()> {
        self.checkpoint().await?;
        write(&self.store, sqlx::query("UPDATE request_logs SET finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),duration_ms=?,outcome=?,error=?,retry_delay_ms=? WHERE id=?")
            .bind(i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX))
            .bind(outcome).bind(error.map(|s|self.redactor.text(s)))
            .bind(retry_delay.map(|d|i64::try_from(d.as_millis()).unwrap_or(i64::MAX)))
            .bind(&self.id)).await?;
        self.done = true;
        Ok(())
    }
}
impl Drop for RequestLog {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // The start row is already durable, even if shutdown kills this update.
        if let Some((capture, content_type)) = &self.response_capture {
            self.response["content"] = capture.snapshot(content_type, &self.redactor);
        }
        self.redactor.value(&mut self.request);
        self.redactor.value(&mut self.response);
        let request = self.request.to_string();
        let response = self.response.to_string();
        let status = self.status.map(i64::from);
        let duration = i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let store = self.store.clone();
        let id = self.id.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error)=write(&store, sqlx::query("UPDATE request_logs SET request=?,response=?,status=?,duration_ms=?,outcome='cancelled',finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),error=COALESCE(error,'request future or response stream dropped before completion') WHERE id=? AND outcome='in_progress'")
                    .bind(request).bind(response).bind(status).bind(duration).bind(&id)).await {
                    tracing::error!(%id,%error,"could not persist cancelled request; start row remains in_progress");
                }
            });
        } else {
            tracing::error!(%id,"request dropped without runtime; durable start row remains in_progress");
        }
    }
}

/// Only called after the server takes the exclusive project lock.
pub(crate) async fn interrupt_leftovers(store: &Store) -> Result<()> {
    write(store, sqlx::query("UPDATE request_logs SET outcome='interrupted',finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),error=COALESCE(error,'previous process ended before request completion; remote outcome unknown') WHERE outcome='in_progress'")
        ).await?;
    Ok(())
}
