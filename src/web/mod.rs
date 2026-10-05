//! Web API adapter (feature `web`): a thin Axum layer over the same store
//! and runtime services the CLI uses.
//!
//! Commands are JSON over POST; live state is Server-Sent Events carrying
//! complete run snapshots derived from SQLite (see [`view`]). The adapter
//! holds no state of its own: every response is rebuilt from durable records,
//! so a restarted server shows exactly what a restarted CLI would.
//!
//! # Security
//!
//! Identity is **one anonymous persistent browser cookie** — no accounts,
//! no login ([`owner`]). Every data route requires it and every run-scoped
//! route enforces owner isolation: a browser only ever sees and controls
//! the runs it created, runs without an owner record (CLI runs) are not
//! reachable over the web at all, and clearing the cookie permanently
//! loses access. The cookie is a bearer token, so transport matters: the
//! server binds `127.0.0.1` by default, and any non-loopback exposure
//! must sit behind a TLS-terminating reverse proxy with
//! `URUK_COOKIE_SECURE=true` (see `docs/WEB.md`). Binding a non-loopback
//! address logs a loud warning but is not refused.

mod error;
mod owner;
mod routes;
mod sse;
mod start;
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
use tower_http::compression::CompressionLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Tunables for the web adapter. Tests shrink the SSE poll interval.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// How often the SSE stream re-reads SQLite for a changed snapshot.
    pub sse_poll: Duration,
    /// Per-request deadline for everything except the SSE stream.
    pub request_timeout: Duration,
    /// Request body cap; commands are small JSON documents.
    pub max_body_bytes: usize,
    /// How often `serve` sweeps for runs it should be driving but is not
    /// (interrupted by a restart, or re-released by `uruk approve`).
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
            max_body_bytes: 64 * 1024,
            reconcile_interval: Duration::from_secs(5),
            cookie_secure: false,
        }
    }
}

struct Inner {
    store: Store,
    provider: Arc<dyn Provider>,
    config: WebConfig,
    /// Runs a scheduler in this process is currently driving, so the
    /// reconcile sweep never dispatches a second scheduler for a run.
    active_runs: Mutex<HashSet<RunId>>,
}

/// Shared state for every handler: the store, the model provider used for
/// runs started over the API, and the adapter tunables.
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
                active_runs: Mutex::new(HashSet::new()),
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

    /// The active-run set, recovering from a poisoned lock: the set stays
    /// usable even if a holder panicked, so the reconcile sweep can never
    /// be wedged into silently skipping every run.
    fn active_runs(&self) -> std::sync::MutexGuard<'_, HashSet<RunId>> {
        self.inner
            .active_runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Claim a run for a scheduler in this process. `false` means another
    /// in-process scheduler already drives it. Never held across `.await`.
    fn claim_run(&self, run_id: &RunId) -> bool {
        self.active_runs().insert(run_id.clone())
    }

    /// Release a claim taken by [`AppState::claim_run`].
    fn release_run(&self, run_id: &RunId) {
        self.active_runs().remove(run_id);
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
    let body_cap = state.config().max_body_bytes;

    let public = Router::new().route("/api/health", get(routes::health));

    let commands = Router::new()
        .route("/api/runs", get(routes::list_runs).post(routes::start_run))
        .route("/api/runs/{run_id}", get(routes::run_snapshot))
        .route("/api/runs/{run_id}/stop", post(routes::stop_run))
        .route("/api/runs/{run_id}/report", get(routes::report))
        .route("/api/runs/{run_id}/sources", get(routes::sources))
        .route("/api/runs/{run_id}/passages", get(routes::passages))
        .route("/api/library", get(routes::library))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            timeout,
        ))
        .layer(CompressionLayer::new());

    let events = Router::new().route("/api/runs/{run_id}/events", get(sse::run_events));

    let identified =
        Router::new()
            .merge(commands)
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
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
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
/// over the API are dispatched inside this process, and SPEC §9.2 allows one
/// scheduler owner per project. `uruk status`, `uruk stop`, and
/// `uruk approve` still work from another terminal; `uruk run` and
/// `uruk resume` fail fast with a lock message while the server owns the
/// project. Because `uruk resume` is locked out, this process also sweeps
/// for runs it should be driving (interrupted `running` runs on startup,
/// and runs released back to work by an approval) on
/// [`WebConfig::reconcile_interval`].
pub async fn serve(opts: ServeOptions) -> Result<()> {
    let _lock = ProjectLock::acquire(&opts.project)?;
    let store = Store::open(opts.project.join(STATE_DB_RELATIVE)).await?;
    let provider = crate::provider::from_env_or_offline()?;
    let state = AppState::new(store, provider, opts.config);

    // Recover immediately, then keep sweeping while the server runs.
    resume_orphaned_runs(&state).await?;
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
    let served = axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(Error::Io);
    reconciler.abort();
    served
}

/// Ctrl-C or SIGTERM ends the accept loop; runs interrupted mid-flight are
/// picked up by [`resume_orphaned_runs`] on the next start (SPEC §9.2).
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
