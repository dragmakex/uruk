//! Web API adapter: a thin Axum layer over the engine's store and runtime
//! services, and the product's only frontend surface.
//!
//! Commands are JSON over POST; live state is Server-Sent Events carrying
//! complete run snapshots derived from SQLite (see [`view`]). The adapter
//! holds no state of its own: every response is rebuilt from durable records,
//! so a restarted server shows exactly the state a fresh one derives.
//!
//! # Security
//!
//! Identity is **one anonymous persistent browser cookie** — no accounts,
//! no login ([`owner`]). Every data route requires it and every run-scoped
//! route enforces owner isolation: a browser only ever sees and controls
//! the runs it created, runs without an owner record (created by tests or
//! by builds that predate the web-only product) are not
//! reachable over the web at all, and clearing the cookie permanently
//! loses access. The cookie is a bearer token, so transport matters: the
//! server binds `127.0.0.1` by default, and any non-loopback exposure
//! must sit behind a TLS-terminating reverse proxy with
//! `URUK_COOKIE_SECURE=true` (see `docs/WEB.md`). Binding a non-loopback
//! address logs a loud warning but is not refused.

mod audit;
mod dispatch;
mod error;
mod owner;
mod routes;
mod sse;
mod start;
mod uploads;
pub mod view;

pub use error::{ApiError, ApiResult};
pub use owner::{COOKIE_NAME, cookie_secure_from_env};
pub use start::{StartRunRequest, resume_orphaned_runs};
pub use view::RunSnapshot;

use crate::provider::Provider;
use crate::records::RunId;
use crate::store::{ProjectLock, STATE_DB_RELATIVE, Store};
use crate::{Error, Result};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tower_http::compression::CompressionLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Tunables for the web adapter. Tests shrink the SSE poll interval.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// How often the SSE stream re-reads SQLite for a changed snapshot.
    pub sse_poll: Duration,
    /// Per-request deadline for ordinary commands (not starts or SSE).
    pub request_timeout: Duration,
    /// Deadline for start and preview requests, which may retrieve sources.
    pub start_timeout: Duration,
    /// Request body cap; commands are small JSON documents.
    pub max_body_bytes: usize,
    /// Body cap for `POST /api/uploads` only: one file per request.
    pub max_upload_bytes: usize,
    /// How many uploads one browser identity may hold at a time.
    pub max_uploads_per_owner: usize,
    /// How often `serve` sweeps for runs that belong on the queue but are
    /// not on it, and the dispatcher's idle tick between wakeups.
    pub reconcile_interval: Duration,
    /// Whether the browser-identity cookie carries the `Secure` attribute.
    /// `false` fits the documented local HTTP setup; any deployment behind
    /// an HTTPS reverse proxy must set it (`URUK_COOKIE_SECURE=true`,
    /// parsed by [`cookie_secure_from_env`]).
    pub cookie_secure: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            sse_poll: Duration::from_millis(750),
            request_timeout: Duration::from_secs(30),
            start_timeout: Duration::from_secs(5 * 60),
            // Validated field maxima can produce just over 100 KiB of JSON.
            max_body_bytes: 128 * 1024,
            max_upload_bytes: 16 * 1024 * 1024,
            max_uploads_per_owner: 32,
            reconcile_interval: Duration::from_secs(5),
            cookie_secure: false,
        }
    }
}

struct Inner {
    store: Store,
    provider: Arc<dyn Provider>,
    config: WebConfig,
    /// Wakes the dispatcher when the queue may have changed. A wakeup
    /// only; the queue itself lives in SQLite.
    queue_wake: Notify,
    /// Runs whose start request is still ingesting sources and planning.
    /// They already hold their queue place (enqueue is part of creation)
    /// but must not execute until the request finishes; a crash empties
    /// this set, and the dispatcher then runs them as they are.
    preparing: Mutex<HashSet<RunId>>,
    /// The one dispatcher task of this server, once started.
    dispatcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The run the dispatcher is executing right now, so the sweep leaves
    /// its tasks to the live scheduler. A hint, not authority: after a
    /// crash it is empty and the sweep settles what the crash orphaned.
    executing: Mutex<Option<RunId>>,
}

/// Shared state for every handler: the store, the model provider used for
/// runs started over the API, the adapter tunables, and the singleton run
/// dispatcher.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

impl AppState {
    pub fn new(store: Store, provider: Arc<dyn Provider>, config: WebConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                provider,
                config,
                queue_wake: Notify::new(),
                preparing: Mutex::new(HashSet::new()),
                dispatcher: Mutex::new(None),
                executing: Mutex::new(None),
            }),
        }
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.inner.provider
    }

    pub fn config(&self) -> &WebConfig {
        &self.inner.config
    }

    /// Start the run dispatcher, the one task that executes the queue.
    /// Returns `false` when it is already running. `serve` starts it; tests
    /// that drive the router directly start it themselves.
    pub fn start_dispatcher(&self) -> bool {
        let mut slot = self
            .inner
            .dispatcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().is_some_and(|task| !task.is_finished()) {
            return false;
        }
        *slot = Some(tokio::spawn(dispatch::run(self.clone())));
        true
    }

    /// Abort the dispatcher. The run it was driving, if any, is left exactly
    /// as a crash would leave it: `running`, head of the queue, resumed by
    /// the next dispatcher.
    pub fn stop_dispatcher(&self) {
        let task = self
            .inner
            .dispatcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.abort();
        }
    }

    /// Tell the dispatcher the queue may have changed.
    pub fn wake_dispatcher(&self) {
        self.inner.queue_wake.notify_one();
    }

    fn queue_wake(&self) -> &Notify {
        &self.inner.queue_wake
    }

    /// The preparing set, recovering from a poisoned lock so the dispatcher
    /// can never be wedged by a panicked start request.
    fn preparing(&self) -> std::sync::MutexGuard<'_, HashSet<RunId>> {
        self.inner
            .preparing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Hold `run_id` back from execution while its start request prepares
    /// it; the returned guard releases it (and wakes the dispatcher) when
    /// dropped, on every exit path. Never held across `.await` itself.
    fn prepare(&self, run_id: &RunId) -> Preparation {
        self.preparing().insert(run_id.clone());
        Preparation {
            state: self.clone(),
            run_id: run_id.clone(),
        }
    }

    fn is_preparing(&self, run_id: &RunId) -> bool {
        self.preparing().contains(run_id)
    }

    fn set_executing(&self, run_id: Option<RunId>) {
        *self
            .inner
            .executing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = run_id;
    }

    fn executing(&self) -> Option<RunId> {
        self.inner
            .executing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Releases a run from the preparing set on drop (see
/// [`AppState::prepare`]).
struct Preparation {
    state: AppState,
    run_id: RunId,
}

impl Drop for Preparation {
    fn drop(&mut self) {
        self.state.preparing().remove(&self.run_id);
        self.state.wake_dispatcher();
    }
}

/// Build the API router.
///
/// The SSE route sits outside the request timeout and response compression:
/// a live stream must neither expire at the request deadline nor be buffered
/// by a compressor.
///
/// Every data route is mounted behind the browser-identity middleware
/// ([`owner::attach_identity`]): handlers reach run-scoped state only
/// through the owner-checked extractors. Health and the unknown-route
/// fallback stay public and never mint a cookie.
pub fn router(state: AppState) -> Router {
    let timeout = state.config().request_timeout;
    let start_timeout = state.config().start_timeout;
    let body_cap = state.config().max_body_bytes;

    let public = Router::new().route("/api/health", get(routes::health));

    let commands = Router::new()
        .route("/api/runs", get(routes::list_runs))
        .route("/api/runs/{run_id}", get(routes::run_snapshot))
        .route("/api/runs/{run_id}/stop", post(routes::stop_run))
        .route("/api/runs/{run_id}/report", get(routes::report))
        .route("/api/runs/{run_id}/report.pdf", get(routes::report_pdf))
        .route("/api/runs/{run_id}/report.md", get(routes::report_markdown))
        .route("/api/runs/{run_id}/sources", get(routes::sources))
        .route(
            "/api/runs/{run_id}/sources/{source_id}",
            get(routes::source_detail),
        )
        .route(
            "/api/runs/{run_id}/sources/{source_id}/passages",
            get(routes::source_passages),
        )
        .route("/api/runs/{run_id}/passages", get(routes::passages))
        .route("/api/runs/{run_id}/citations", get(routes::citations))
        .route("/api/library", get(routes::library))
        .route("/api/library/passages", get(routes::library_passages))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            timeout,
        ))
        .layer(axum::middleware::from_fn_with_state(
            timeout,
            reject_zero_deadline,
        ));

    // Explicit source retrieval has its own bounded timeout and may exceed
    // the ordinary command deadline. Do not return 408 after the run row
    // has committed while initialization continues without a client-known id.
    let starts = Router::new()
        .route("/api/runs", post(routes::start_run))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            start_timeout,
        ))
        .layer(axum::middleware::from_fn_with_state(
            start_timeout,
            reject_zero_deadline,
        ));

    // Uploads carry whole files, so this route group alone gets a larger
    // body cap (the route-level limit overrides the router-wide one).
    let upload_routes = Router::new()
        .route("/api/uploads", get(uploads::list).post(uploads::create))
        .route(
            "/api/uploads/{upload_id}",
            axum::routing::delete(uploads::remove),
        )
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            timeout,
        ))
        .layer(axum::middleware::from_fn_with_state(
            timeout,
            reject_zero_deadline,
        ))
        .layer(DefaultBodyLimit::max(state.config().max_upload_bytes));

    let events = Router::new().route("/api/runs/{run_id}/events", get(sse::run_events));

    let identified = Router::new()
        .merge(commands)
        .merge(starts)
        .merge(upload_routes)
        .merge(events)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            owner::attach_identity,
        ));

    Router::new()
        .merge(public)
        .merge(identified)
        .fallback(routes::unknown_route)
        .layer(DefaultBodyLimit::max(body_cap))
        // Errors produced below the handlers (timeouts, method mismatches)
        // have empty bodies; rewrite them into the stable JSON error shape.
        .layer(axum::middleware::map_response(error::ensure_json_error))
        // Request-id plumbing: set on the way in (outermost), propagate onto
        // the response on the way out, with tracing in between.
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            audit::record,
        ))
        // Compress only after diagnostics captures the uncompressed response.
        // The default compression predicate excludes text/event-stream.
        .layer(CompressionLayer::new())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}

/// A zero deadline must never poll a mutating handler. Tower's timeout races
/// a zero-duration timer against a ready handler, otherwise allowing side
/// effects or a cancellation in the middle of initializing a transaction.
async fn reject_zero_deadline(
    axum::extract::State(deadline): axum::extract::State<Duration>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if deadline.is_zero() {
        return axum::response::IntoResponse::into_response(
            axum::http::StatusCode::REQUEST_TIMEOUT,
        );
    }
    next.run(request).await
}

/// How `uruk serve` runs the adapter.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub bind: SocketAddr,
    pub project: PathBuf,
    pub config: WebConfig,
}

/// Serve the API until Ctrl-C / SIGTERM.
///
/// Takes the project scheduler lock for its whole lifetime: runs started
/// over the API are executed by this process's dispatcher, one at a time
/// in queue order, and SPEC §9.2 allows one scheduler owner per project,
/// so a second `uruk serve` on the same project fails fast with a lock
/// message. Because this process is the only scheduler owner, it also
/// sweeps for runs that belong on the queue but are not on it on
/// [`WebConfig::reconcile_interval`]. A restart changes nothing about the
/// queue: the interrupted head is resumed first, then the rest in order.
pub async fn serve(opts: ServeOptions) -> Result<()> {
    let _lock = ProjectLock::acquire(&opts.project)?;
    let store = Store::open(opts.project.join(STATE_DB_RELATIVE)).await?;
    crate::diagnostics::interrupt_leftovers(&store).await?;
    let provider = crate::provider::from_env_or_offline()?;
    let state = AppState::new(store, provider, opts.config);

    // Put stragglers on the queue, start executing it, and keep sweeping
    // while the server runs.
    resume_orphaned_runs(&state).await?;
    state.start_dispatcher();
    let reconciler = tokio::spawn({
        let state = state.clone();
        async move {
            let mut tick = tokio::time::interval(state.config().reconcile_interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(e) = resume_orphaned_runs(&state).await {
                    tracing::error!(error = %e, "reconcile sweep failed");
                }
            }
        }
    });

    if !opts.bind.ip().is_loopback() {
        // Deliberate exposure behind a reverse proxy is the only sane
        // reason to see this.
        tracing::warn!(
            addr = %opts.bind,
            "binding a NON-LOOPBACK address: browser isolation rests entirely on an \
             anonymous bearer cookie; front this with a TLS-terminating reverse proxy \
             and keep the port itself unreachable (docs/WEB.md)"
        );
        if !state.config().cookie_secure {
            tracing::warn!(
                "URUK_COOKIE_SECURE is not set: the identity cookie will also be sent \
                 over plain HTTP. Set URUK_COOKIE_SECURE=true behind an HTTPS proxy \
                 (docs/WEB.md); local HTTP development is the only setup where leaving \
                 it off is sound"
            );
        }
    }

    let listener = tokio::net::TcpListener::bind(opts.bind).await?;
    tracing::info!(addr = %opts.bind, "uruk web API listening");
    let served = axum::serve(listener, router(state.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(Error::Io);
    reconciler.abort();
    state.stop_dispatcher();
    served
}

/// Ctrl-C or SIGTERM ends the accept loop; a run interrupted mid-flight is
/// still the head of the durable queue and is resumed by the next start's
/// dispatcher (SPEC §9.2).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutdown requested; stopping the web API");
}
