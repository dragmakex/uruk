# The Uruk web interface

Uruk ships a local-first web console, and the browser is the product's
only frontend: a Rust API served by the `uruk` binary itself (whose
single command is `uruk serve`), and a separate Next.js frontend under
`web/`. The engine and its SQLite store remain authoritative; the web
layer is a thin adapter that derives every response from the engine's
durable records.

## Security: read this first

Identity is **one anonymous persistent browser cookie** — deliberately no
accounts, no login, no email, no OAuth. On the first API request that
needs an identity, the Rust service mints a 256-bit random token, sets it
as `uruk_browser` (host-only, `HttpOnly`, `Path=/`, `SameSite=Lax`,
`Max-Age` one year), and from then on:

- A browser only ever sees and controls the runs it created: listing,
  snapshots, reports, sources, passages, the SSE stream, and stop are all
  owner-scoped, enforced in the Rust store. Foreign and unknown run ids
  get the same `not_found` answer, so probing discloses nothing.
- Runs without an owner record (created by builds that predate the
  web-only product, or directly through the engine by tests) are **not
  reachable over the web at all** — they cannot be claimed by a visitor;
  web access fails closed.
- SQLite stores only the SHA-256 digest of the token, never the token,
  so a copy of the database grants access to nobody's runs. The token is
  never logged and never visible to frontend JavaScript.
- Clearing the cookie (or site data) **permanently loses access** to that
  browser's runs. There is no recovery and no cross-device access; that
  is the intended contract, not an accident.

What this is **not**: a user system. The cookie is a bearer token — any
client presenting it is that browser. Therefore transport still matters:

- `uruk serve` binds `127.0.0.1:7913` by default. Leave it there for
  local use.
- Any non-loopback exposure (`--bind 0.0.0.0:7913` logs a loud warning)
  must sit behind a TLS-terminating reverse proxy, with
  `URUK_COOKIE_SECURE=true` set so browsers refuse to send the identity
  cookie over plain HTTP (see the configuration table below).
- Everything behind one API still shares one project and one
  model-provider budget: ownership isolates *visibility and control of
  runs*, not spending. Starting runs is open to every visitor, so do not
  put this in front of the anonymous internet unless you accept that.

## Architecture

```
browser ── same-origin /api ──▶ Rust API (axum, uruk serve, :7913)
   │                                 │
   └── pages, assets ──▶ Next.js (web/, :3000)   SQLite (.uruk/state.sqlite)
```

- **Commands are JSON over POST** (`/api/runs`, `/api/runs/{id}/stop`).
- **Live state is Server-Sent Events** (`/api/runs/{id}/events`): the
  server re-derives a complete run snapshot from SQLite on a short
  interval and emits it when it changed. The browser always holds one
  coherent snapshot; there is no granular event protocol to replay. When
  the run ends the stream sends a final snapshot, an `end` event, and
  closes.
- The browser only ever talks to the same-origin `/api` path. In
  development, `next dev` rewrites `/api/*` to the Rust API. In
  production, the reverse proxy routes `/api` to the Rust service before
  requests reach Next. Either way the identity cookie is set by and
  returned to the Rust service directly — Next never handles the token,
  and `HttpOnly` keeps it away from frontend JavaScript. Same-origin
  fetches and the `EventSource` stream carry it automatically.
- Server Components fetch the Rust API directly at `URUK_API_URL`
  (default `http://127.0.0.1:7913`) for first paint; the live run page
  then follows SSE from the client. Because those fetches bypass the
  public origin, the pages forward the incoming request's cookie header
  by hand (`web/lib/server-identity.ts`). Server Components can read but
  never set cookies, so a brand-new visitor's first paint renders as an
  empty workspace and the browser's own first `/api` request mints the
  durable cookie.
- Errors use one stable JSON body everywhere:
  `{"ok": false, "error": <prose>, "kind": <kind>}` with the engine's
  stable `kind` strings (`validation`, `not_found`,
  `permission`, `budget`, `storage`, …), plus `timeout` for a request
  that hit the server-side deadline. Infrastructure failures (the 5xx
  family) are logged in full on the server but reported with generic
  prose: internal paths and SQL state never cross the wire.

The Rust adapter lives in `src/web/`. It holds no state of its own;
every response is rebuilt from durable records, so a restarted server
shows exactly the state a fresh one derives.

Runs started over the API are executed inside the serve process, one at
a time (see "One run at a time" below). `uruk serve` therefore takes the
project scheduler lock for its lifetime: a second `uruk serve` on the
same project fails fast with a lock message until the first stops. As
the project's only scheduler owner, the serve process also sweeps every
few seconds for runs that belong on the queue but are not on it: a run
parked `waiting-for-human` is re-enqueued once no approvals remain
pending. Web-started runs accept the full research configuration —
rubric guidance, budgets, source URLs, file inputs, and the explicit
network and literature-search grants — but computational execution and
arbitrary tool names remain unavailable. The web surface has no
exact-payload approve/deny step to bind them to; an anonymous cookie
identifies a browser, not a person who can be held to an approval. No
request field ever names a server filesystem path: files enter web runs
only through owner-bound uploads (`POST /api/uploads`), stored under a
server-chosen location that never crosses the wire.

## One run at a time

Every browser shares one model provider, so web runs execute **one at a
time**: in the order they were started within one browser, and taking
turns between browsers (see "Taking turns" below):

- `POST /api/runs` checks the goal against the safety rules first; a
  refused goal is persisted `blocked`, with its decision, and never takes
  a queue place. An accepted goal creates the run, its goal, its owner
  binding, and its place on the run queue in **one SQLite transaction**
  (table `run_queue`, migration 0006). The place is a dedicated sequence
  number assigned at creation, never a timestamp, so two starts a
  millisecond apart still have a definite order and the order survives a
  restart. The place is `preparing` while the request ingests sources and
  plans, and becomes executable (`queued`) in the same transaction that
  records the plan. A request that dies first — a server crash, the
  request timeout — leaves a `preparing` place the dispatcher abandons:
  the run ends `failed` with stop condition `start_incomplete` and a
  recorded decision, never executed, and the queue moves on. The run is
  in state `queued` until it reaches the head; it holds no tasks yet and
  spends nothing.
- **One dispatcher** in the serve process executes the *fair head* of the
  queue and only that: it activates the entry (advancing the turn cursor
  in the same transaction), runs the scheduler to the end, exports the
  report, and closes the entry. Later runs **never overtake** a head that
  is waiting for the provider.
- A **transient provider failure** (a connect, send, or response transport
  error, including completion timeouts; or a 408, 425,
  429, or 5xx) does not fail anything. The head stays
  `running`, its model call waits for the upstream to recover behind a
  process-wide availability gate (one half-open probe at a time,
  exponential backoff capped at 30 s, `Retry-After` honored), and its
  research clock stands still: the wall-clock budget runs from the run's
  first start and excludes every provider wait. Only served calls count
  against `max_model_calls`; unserved sends are audited as `resend` rows
  in the budget ledger. A permanent refusal (another 4xx, an unparseable
  reply) is terminal for that task at once. Completion timeouts instead
  enter the same recovery loop; each send is bounded, and Stop cancels both
  in-flight requests and backoff waits. Retries can incur unknown provider cost.
- Campaign rounds are scheduling batches, not a count of tournament matches.
  A batch of acquisitions or prerequisite work may leave research counters
  unchanged. That is not a stopping condition: pending work is dispatched and
  new useful work is selected until an explicit budget/round limit, cancellation,
  safety boundary, or actual work exhaustion is reached.
- **Stop** always works and is one transaction: the run is cancelled and
  leaves the queue, its undispatched tasks are cancelled with their
  reservations released, and the decision is recorded together, so a
  crash leaves the whole stop or none of it. A head waiting for the
  provider is interrupted promptly rather than after the next probe. Tasks
  a crash orphans `running` after a stop are settled by the sweep: charged
  for the calls they had started, at unknown cost, and closed.
- **Restart**: a run left `running` by an interrupted process is still the
  head of the durable queue and is resumed first; queued runs follow in
  order. A head whose scheduler fails on a store fault keeps its place and
  is retried with bounded backoff rather than released and re-queued at
  the tail. A provider wait that was open at shutdown is persisted and
  settled when the provider serves again.

### Taking turns

Between browsers the queue serves the **least recently served** browser
next, each browser's runs in the order it started them. Six runs started
as `A1 A2 A3 B1 C1 B2` execute as `A1 B1 C1 A2 B2 A3`. Two durable facts
drive this: a global dispatch turn (`queue_turns`, incremented on every
activation) and each browser's last-served turn (`queue_owners`; a
browser never served is the most deserving). Selection takes each
browser's *oldest* waiting run as its candidate — a later run never
passes an earlier one of the same browser — and picks the eligible
browser with the smallest last-served turn, ties broken by start order.
Activation reselects, increments the turn, stamps the browser, and claims
the run in one transaction, so a restart continues the turn order exactly
where it was, and an interrupted active run always resumes before any
queued run. A browser that starts its first run while another browser's
run is active is served at the next turn, never by interrupting the
active run; a browser whose runs all finished and that starts another
waits behind every browser served less recently. A run still being
prepared makes its browser ineligible until it is ready (its later runs
cannot overtake it) without holding any other browser back; once ready,
that browser gets one turn like any other, never a catch-up burst. Runs
without a browser identity — engine callers, runs re-enqueued by the
sweep, runs predating ownership — take turns as one owner named
`engine`, in start order among themselves. Browser identities are stored
only as one-way digests and are never served over the API.

The API tells the truth about all of this. The run snapshot's `run`
object carries `queue_position` — its place in the projected execution
order: position 1 is the run that would be selected right now, and a run
whose browser is not currently eligible (still preparing) is listed after
every run that could be selected. The projection reflects readiness at
this moment and changes when a preparation completes or a run is
cancelled. It is `1` = next to start and `null` once the run executes.
The object also carries `started_at` (`null` while queued; the budget runs from it),
`paused_secs` (settled provider waits, excluded from the research clock),
`provider_wait` (`{since, note}` while an outage is being waited out,
else `null`), and `queue_provider_waiting` — a required boolean: while
this run holds a queue place, whether the run being executed is waiting
out a provider outage, so a queued researcher knows why nothing moves.
It is one bit by design: a follower never learns which run is waiting,
whose it is, when its outage began, or what the provider said. It is
`false` once the provider serves again, when nothing is active, and for
any run without a queue place. `queue_position` and
`queue_provider_waiting` are read in one SQLite transaction
(`Store::queue_context`), so a snapshot never pairs an old projection
with a newer head's wait, and both come from persisted facts, so a
restart reports them unchanged. The console treats a non-null
`queue_position` as "waiting", whatever the lifecycle state says: a
recovered `running` or `waiting-for-human` run that was put back on
the queue is shown queued, with no figure until it executes. `GET
/api/runs` rows carry `queue_position` and `paused_since`. Run states are `queued`, `running`, `waiting-for-human`,
`blocked`, `completed`, `budget-exhausted`, `cancelled`, and `failed`;
queued and provider-waiting runs are never reported as failed. A run that
was abandoned before it was ready reports `failed` with stop condition
`start_incomplete`.

Ownership lives in one additive table, `run_owners` (migration 0004):
`run_id → owner_digest`, written in the same transaction that creates a
web run, so a crash can never leave an unowned web run behind. Existing
databases migrate untouched — old runs simply have no row, which web
reads treat as nonexistent. Enforcement is centralized in two axum
extractors (`src/web/owner.rs`): run-scoped handlers receive an already
authorized run id or the request never reaches them, so a new handler
cannot quietly skip the check.

## API surface

| Route | Meaning |
| --- | --- |
| `GET /api/health` | liveness and version; public, never sets a cookie |
| `GET /api/runs` | this browser's runs, with goal, state, queue position, and provider wait |
| `POST /api/runs` | start a run owned by this browser: `{goal, mode?, ranking?, …}`; it is enqueued, not executed at once |
| `GET /api/runs/{id}` | complete run snapshot (view model) |
| `POST /api/runs/{id}/stop` | durable stop request; works on queued and provider-waiting runs too |
| `GET /api/runs/{id}/events` | SSE stream of snapshots |
| `GET /api/runs/{id}/report` | exported `REPORT.md` + `manifest.json` |
| `GET /api/runs/{id}/report.pdf` | `REPORT.pdf` download (derived on demand for pre-PDF exports) |
| `GET /api/runs/{id}/report.md` | `REPORT.md` download, canonical bytes verbatim |
| `GET /api/runs/{id}/sources` | the run's recorded sources |
| `GET /api/runs/{id}/sources/{sid}` | one source with its indexed passage count |
| `GET /api/runs/{id}/sources/{sid}/passages` | browse (`offset`/`limit`) or search (`q`) one source's passages |
| `GET /api/runs/{id}/passages?q=…` | local FTS5 passage search |
| `GET /api/runs/{id}/citations` | every persisted citation with its honest resolution |
| `GET /api/library` | all sources across this browser's runs; filters `access`, `origin`, `q` |
| `GET /api/library/passages?q=…` | BM25 passage search across every owned run |
| `POST /api/uploads?name=…` | store raw request bytes as an owner-bound file |
| `GET /api/uploads` | this browser's uploads, newest first |
| `DELETE /api/uploads/{id}` | remove an upload this browser owns |

Every route except `/api/health` requires the browser identity: a request
without a valid `uruk_browser` cookie gets one minted (and used for that
same request) via `Set-Cookie`. All `{id}` routes are owner-checked; a
run belonging to another browser is answered exactly like a run that
does not exist. Source-scoped routes additionally require the source to
belong to that run — both constraints live in the SQL queries themselves
(and in the `OwnedSource` extractor), never in a post-fetch check, so a
foreign run, a foreign source, and a nonexistent one are indistinguishable.

### The library explorer and honest citations

The library pages (`/library`, `/library/search`, and the per-source page
under each run) expose only recorded facts: the provenance a run stored
for each source, and the passages chunked from the extracted-text
artifact with their exact byte offsets into the canonical UTF-8 text.
`GET /api/runs/{id}/citations` resolves every persisted citation —
deliverable claim groundings and review observations — into exactly one
of three honest shapes: a `span` whose bytes are served only after the
artifact still matches its recorded content hash and the offsets fall on
character boundaries; a `source_locator` when the citation names a
recorded source at a coarse locator (a page, a section); or `unresolved`
with the reason stated plainly (unknown source id, drifted artifact,
out-of-bounds or mid-character span). Nothing is approximated or
repaired, and all byte slicing happens in Rust — the frontend renders
resolved text, it never slices offsets itself (`src/report/citations.rs`).

`POST /api/runs` accepts the full safe start configuration
(`src/web/start.rs` is the authority):

- `mode` (`task` default, `campaign`) and `ranking` (`simple` default,
  `tournament`). Both ranking options keep the Elo tournament; the
  stored difference is the §7 debate turn cap, which is what
  "single-turn comparison" versus "multi-turn scientific debate" means
  in this engine. `max_debate_turns` (2–10, default 5) therefore only
  combines with `tournament`.
- `profile`, `deliverables`, `preferences`, `attributes`, `constraints`:
  the goal record and rubric, as on the CLI.
- Budgets: `max_model_calls`, `max_seconds`, `max_iterations`,
  `max_acquisitions`, validated by the API. `max_seconds` defaults to `0`,
  meaning **no time limit**; a positive value (1–604800 seconds) opts into
  an elapsed-time deadline. The browser also defaults to 0 and labels it
  explicitly. Model-call and iteration limits still apply independently.
  Individual provider request timeouts remain bounded and retryable.
  Existing runs retain their recorded budgets; completed or exhausted runs
  are not automatically restarted by this default change.
- Sources: `input_urls` (`http(s)` only — a path-like value is refused)
  and `upload_ids` referencing this browser's uploads. A foreign and an
  unknown upload id get one identical validation error.
- Grants, all off by default: `allow_network` permits retrieval of the
  supplied URLs; `search` opts into federated literature search
  (OpenAlex, Crossref, arXiv) and requires `allow_network` because it
  transmits goal-derived queries to those operators;
  `search_connectors` narrows the connector set. Execution is never
  grantable; unknown fields are refused outright.
- `dry_run: true` validates, safety-checks, and plans without creating
  anything, returning the plan, inputs, permissions, and budget the
  start would use.

Request bodies are capped at 128 KiB — except `POST /api/uploads`, which
carries raw file bytes under its own per-file limit (16 MiB default)
plus a per-owner count bound (32 default) and a fixed allowlist of
text-extractable file types. Every response carries an `x-request-id`.

## Local development

Two services, two terminals:

```sh
# 1. The API (from the project directory you want to work in):
cargo run -- serve                  # binds 127.0.0.1:7913

# 2. The frontend:
cd web
bun install
bun run dev                         # binds localhost:3000, proxies /api
```

Open `http://localhost:3000`. A model provider is configured through the
environment (`URUK_PROVIDER_URL`, `URUK_MODEL`, `URUK_API_KEY`, and optional
`URUK_PROVIDER_TIMEOUT_SECS`, read from the project's `.env` when `uruk serve`
starts); without one, runs
complete offline with a stand-in that states plainly that nothing was
analysed.

Environment variables:

| Variable | Read by | Default | Meaning |
| --- | --- | --- | --- |
| `URUK_API_URL` | Next | `http://127.0.0.1:7913` | where Next (server side and the dev rewrite) reaches the Rust API |
| `URUK_COOKIE_SECURE` | `uruk serve` | `false` | mark the identity cookie `Secure` so browsers only send it over HTTPS. Accepts exactly `true`/`1`/`false`/`0`; anything else refuses to start. Leave unset for the loopback HTTP setup on this page — that is a documented local-development trade-off, **not** a production-safe default. Set `true` for any HTTPS deployment |

Both live naturally in the project's `.env` next to the provider
settings.

## Production, same origin

Build and run both services, then put a reverse proxy in front so the
browser sees one origin:

```sh
cargo build --release && ./target/release/uruk serve
cd web && bun run build && bun run start -p 3000
```

Caddy:

```caddy
localhost:8080 {
    handle /api/* {
        reverse_proxy 127.0.0.1:7913
    }
    handle {
        reverse_proxy 127.0.0.1:3000
    }
}
```

nginx (the SSE route must not be buffered):

```nginx
server {
    listen 127.0.0.1:8080;

    location /api/ {
        proxy_pass http://127.0.0.1:7913;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_buffering off;          # SSE: pass events through immediately
        proxy_read_timeout 1h;        # SSE streams outlive normal timeouts
    }

    location / {
        proxy_pass http://127.0.0.1:3000;
    }
}
```

Cookie expectations at the proxy: the identity cookie is set by the Rust
service on `/api` responses and must reach the browser untouched, so the
proxy must not strip or rewrite `Set-Cookie`/`Cookie` headers on the
`/api` path (neither Caddy nor nginx does by default). The cookie is
host-only — it belongs to the one public origin the proxy presents — and
the SSE route authenticates with the same cookie, which `EventSource`
sends automatically on the same origin. If the proxy terminates TLS, set
`URUK_COOKIE_SECURE=true` on `uruk serve`; the Rust service itself can
stay on plain loopback HTTP behind it, since the attribute only tells
the *browser* when to send the cookie.

Keep the proxy itself on loopback or otherwise unreachable to strangers;
see the warning at the top — every visitor shares one project and one
model budget. This repository documents a verified local setup only;
nothing here has been deployed.

## Testing

- Rust: `cargo test --locked` includes `tests/web_api.rs`, which drives
  the router through `tower::ServiceExt::oneshot` against temporary
  SQLite stores and the mock provider: view-model projection, validation,
  stable error bodies, stop, report, passages, body caps, request IDs,
  and SSE (initial snapshot, terminal settle). `tests/web_owner.rs`
  covers the identity layer with independent cookie jars: cookie minting
  and attributes, malformed-cookie replacement, cross-browser isolation
  on every run-scoped route including SSE and stop, foreign/unknown
  indistinguishability, ownerless-run invisibility, restart persistence, and
  digest-only storage. `tests/web_uploads.rs` covers the upload store:
  creation, listing, deletion, owner isolation, size/count/type bounds,
  and path-shaped name refusal. `tests/web_start_config.rs` covers the
  full start configuration: goal-record round-trips, grant semantics,
  upload ingestion, dry-run previews, and the refusal of execution and
  path fields. Everything is offline.
- Frontend: `cd web && bun run check` runs ESLint, `tsc --noEmit`, Vitest
  (runtime parsers, formatting, the SSE reducer, form validation, agent
  cards, the live run view with a scripted EventSource, including the
  queued and provider-waiting states), and the production build.
  `tests/run_queue_store.rs`, `tests/provider_availability.rs`, and
  `tests/web_run_queue.rs` cover the durable queue, the availability
  gate, and the dispatcher end to end, offline.
