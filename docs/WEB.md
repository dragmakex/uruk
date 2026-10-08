# The Uruk web interface

Uruk ships a local-first web console: a Rust API served by the `uruk`
binary itself, and a separate Next.js frontend under `web/`. The engine and
its SQLite store remain authoritative; the web layer is a thin adapter that
reads the same records the CLI reads and issues the same commands the CLI
issues.

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
- Runs created by the CLI (and any run from before ownership existed)
  have no owner record and are **not reachable over the web at all** —
  they cannot be claimed by a visitor; web access fails closed.
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
   └── pages, assets ──▶ Next.js (web/, :3000)   SQLite ◀── CLI (uruk …)
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
  `{"ok": false, "error": <prose>, "kind": <kind>}` with the same `kind`
  strings as the CLI's `--json` mode (`validation`, `not_found`,
  `permission`, `budget`, `storage`, …), plus `timeout` for a request
  that hit the server-side deadline. Infrastructure failures (the 5xx
  family) are logged in full on the server but reported with generic
  prose: internal paths and SQL state never cross the wire.

The Rust adapter lives in `src/web/` behind the on-by-default `web` cargo
feature (`--no-default-features` builds the CLI-only binary). It holds no
state of its own; every response is rebuilt from durable records, so a
restarted server shows exactly what a restarted CLI would.

Runs started over the API are dispatched by a scheduler inside the serve
process. `uruk serve` therefore takes the project scheduler lock for its
lifetime: `uruk status`, `uruk stop`, and `uruk approve` keep working
from another terminal, while `uruk run` and `uruk resume` fail fast with
a lock message until the server stops. Because `uruk resume` is locked
out, the serve process sweeps every few seconds for runs it should be
driving: runs left `running` by an interrupted process are resumed on
startup, and runs parked `waiting-for-human` are picked back up once
their approvals are decided. Web-started runs accept the full research
configuration — rubric guidance, budgets, source URLs, file inputs, and
the explicit network and literature-search grants — but two grants stay
CLI-only on purpose: local subprocess execution and arbitrary tool
names. The web surface has no exact-payload approve/deny step to bind
them to; an anonymous cookie identifies a browser, not a person who can
be held to an approval. No request field ever names a server filesystem
path: files enter web runs only through owner-bound uploads
(`POST /api/uploads`), stored under a server-chosen location that never
crosses the wire.

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
| `GET /api/runs` | this browser's runs, with goal and state |
| `POST /api/runs` | start a run owned by this browser: `{goal, mode?, ranking?, …}` |
| `GET /api/runs/{id}` | complete run snapshot (view model) |
| `POST /api/runs/{id}/stop` | durable stop request |
| `GET /api/runs/{id}/events` | SSE stream of snapshots |
| `GET /api/runs/{id}/report` | exported `REPORT.md` + `manifest.json` |
| `GET /api/runs/{id}/sources` | the run's recorded sources |
| `GET /api/runs/{id}/passages?q=…` | local FTS5 passage search |
| `GET /api/library` | all sources across this browser's runs |
| `POST /api/uploads?name=…` | store raw request bytes as an owner-bound file |
| `GET /api/uploads` | this browser's uploads, newest first |
| `DELETE /api/uploads/{id}` | remove an upload this browser owns |

Every route except `/api/health` requires the browser identity: a request
without a valid `uruk_browser` cookie gets one minted (and used for that
same request) via `Set-Cookie`. All `{id}` routes are owner-checked; a
run belonging to another browser is answered exactly like a run that
does not exist.

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
  `max_acquisitions`, each validated against the same bounds the CLI
  enforces.
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

Open `http://localhost:3000`. A model provider is configured exactly as
for the CLI (`URUK_PROVIDER_URL`, `URUK_MODEL`, `URUK_API_KEY`, read from
the project's `.env`); without one, runs complete offline with a stand-in
that states plainly that nothing was analysed.

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
  indistinguishability, CLI-run invisibility, restart persistence, and
  digest-only storage. `tests/web_uploads.rs` covers the upload store:
  creation, listing, deletion, owner isolation, size/count/type bounds,
  and path-shaped name refusal. `tests/web_start_config.rs` covers the
  full start configuration: goal-record round-trips, grant semantics,
  upload ingestion, dry-run previews, and the refusal of execution and
  path fields. Everything is offline.
- Frontend: `cd web && bun run check` runs ESLint, `tsc --noEmit`, Vitest
  (runtime parsers, formatting, the SSE reducer, form validation, agent
  cards, the live run view with a scripted EventSource), and the
  production build.
