//! Streaming request/response capture: never buffers an SSE stream or upload
//! before forwarding it. SQLite sees the start before the handler runs.

#[cfg(test)]
mod tests;
use super::{
    AppState,
    error::{ApiError, DiagnosticError},
};
use crate::diagnostics::{self, Capture, Meta, Redactor, RequestLog, SharedCapture, capture_lock};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::Stream;
use serde_json::json;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

type BodyStream = Pin<Box<dyn Stream<Item = std::result::Result<Bytes, axum::Error>> + Send>>;
type Finish = Pin<Box<dyn Future<Output = crate::Result<()>> + Send>>;
struct Tap {
    inner: BodyStream,
    capture: SharedCapture,
    completion: Option<Completion>,
    finishing: Option<Finish>,
    done: bool,
    terminal_error: Option<std::io::Error>,
}
struct Completion {
    log: RequestLog,
    request: SharedCapture,
    request_type: String,
    response_type: String,
}
impl Completion {
    fn snapshot(&mut self, response: &SharedCapture) {
        let request = capture_lock(&self.request);
        self.log.redactor.observe_capture(&request);
        self.log.request["content"] = request.snapshot(&self.request_type, &self.log.redactor);
        self.log.response["content"] =
            capture_lock(response).snapshot(&self.response_type, &self.log.redactor);
    }
}
impl Tap {
    fn body(body: Body, capture: SharedCapture, completion: Option<Completion>) -> Body {
        Body::from_stream(Self {
            inner: Box::pin(body.into_data_stream()),
            capture,
            completion,
            finishing: None,
            done: false,
            terminal_error: None,
        })
    }
    fn finish(&mut self, error: Option<String>) {
        self.done = true;
        if let Some(mut completion) = self.completion.take() {
            completion.snapshot(&self.capture);
            self.finishing = Some(Box::pin(async move {
                let status = completion.log.status.unwrap_or(200);
                let outcome = if error.is_some() {
                    "stream_error"
                } else if status >= 400 {
                    "http_error"
                } else {
                    "success"
                };
                let detail = error.or_else(|| {
                    completion
                        .log
                        .response
                        .get("internal_error")
                        .and_then(|e| e.as_str())
                        .map(str::to_owned)
                        .or_else(|| {
                            completion
                                .log
                                .response
                                .pointer("/content/body/error")
                                .and_then(|e| e.as_str())
                                .map(str::to_owned)
                        })
                });
                completion
                    .log
                    .finish(outcome, detail.as_deref(), None)
                    .await
            }));
        }
    }
}
impl Stream for Tap {
    type Item = std::result::Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(future) = &mut self.finishing {
                match future.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => {
                        self.finishing = None;
                        if let Err(error) = result {
                            tracing::error!(%error,"request diagnostic finalization failed");
                            self.terminal_error = Some(std::io::Error::other(error.to_string()));
                        }
                    }
                }
            }
            if self.done {
                return Poll::Ready(self.terminal_error.take().map(Err));
            }
            match self.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(bytes))) => {
                    capture_lock(&self.capture).push(&bytes);
                    return Poll::Ready(Some(Ok(bytes)));
                }
                Poll::Ready(Some(Err(error))) => {
                    let detail = diagnostics::error_chain(&error);
                    capture_lock(&self.capture).error = Some(detail.clone());
                    self.terminal_error = Some(std::io::Error::other(detail.clone()));
                    self.finish(Some(detail));
                }
                Poll::Ready(None) => {
                    capture_lock(&self.capture).complete = true;
                    self.finish(None);
                }
            }
        }
    }
}
impl Drop for Tap {
    fn drop(&mut self) {
        if let Some(mut completion) = self.completion.take() {
            completion.snapshot(&self.capture);
            // RequestLog's drop records cancellation, including partial bodies.
        }
    }
}
fn content_type(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

pub(super) async fn record(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    match record_inner(state, request, next).await {
        Ok(response) => response,
        Err(error) => ApiError(error).into_response(),
    }
}
async fn record_inner(state: AppState, request: Request, next: Next) -> crate::Result<Response> {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unassigned")
        .to_owned();
    let mut redactor = Redactor::from_headers(request.headers());
    redactor.observe_url(&request.uri().to_string());
    let request_type = content_type(request.headers());
    let run_id = request
        .uri()
        .path()
        .strip_prefix("/api/runs/")
        .and_then(|s| s.split('/').next())
        .map(str::to_owned);
    let meta = Meta {
        direction: "inbound",
        request_id,
        run_id,
        ..Default::default()
    };
    let request_detail = json!({"method":request.method().as_str(),"url":diagnostics::url(&request.uri().to_string()),"headers":diagnostics::headers(request.headers())});
    let mut log = RequestLog::begin(state.store().clone(), meta, request_detail, redactor).await?;
    let capture = Arc::new(Mutex::new(Capture::default()));
    let (parts, body) = request.into_parts();
    let request = Request::from_parts(parts, Tap::body(body, capture.clone(), None));
    let response = next.run(request).await;
    log.status = Some(response.status().as_u16());
    {
        let captured = capture_lock(&capture);
        log.redactor.observe_capture(&captured);
        log.request["content"] = captured.snapshot(&request_type, &log.redactor);
    }
    log.response = json!({"headers":diagnostics::headers(response.headers())});
    if let Some(error) = response.extensions().get::<DiagnosticError>() {
        log.response["internal_error"] = json!(error.0);
    }
    // Long-lived streams already have a persisted status and consumed request.
    log.checkpoint().await?;
    let response_type = content_type(response.headers());
    let (parts, body) = response.into_parts();
    let completion = Completion {
        log,
        request: capture,
        request_type,
        response_type,
    };
    Ok(Response::from_parts(
        parts,
        Tap::body(
            body,
            Arc::new(Mutex::new(Capture::default())),
            Some(completion),
        ),
    ))
}
