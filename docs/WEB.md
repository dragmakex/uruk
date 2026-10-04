# The Uruk web interface

Uruk ships a local-first web console: a Rust API served by the `uruk`
binary itself, and a separate Next.js frontend under `web/`. The engine and
its SQLite store remain authoritative; the web layer is a thin adapter that
reads the same records the CLI reads and issues the same commands the CLI
issues.

## Security: read this first

**The API has no authentication, no accounts, and no multi-tenant
authorization. Anyone who can reach the port controls the project, its
data, and its model-provider budget.**

Consequences:

- `uruk serve` binds `127.0.0.1:7913` by default. Leave it there.
- Binding a non-loopback address (`--bind 0.0.0.0:7913`) logs a loud
  warning and must only be done behind a reverse proxy that terminates
  authentication, and only for yourself: there is no per-user isolation,
  so two people behind the same proxy share one project and can read and
  cancel each other's runs.
- Do not expose either service to the public internet until authentication
  and multi-user authorization exist. They are deliberately deferred.

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
  requests reach Next.
- Server Components fetch the Rust API directly at `URUK_API_URL`
  (default `http://127.0.0.1:7913`) for first paint; the live run page
  then follows SSE from the client.
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
their approvals are decided. Web-started runs grant no permissions
beyond provider disclosure: no file inputs, no network retrieval, no
execution, no literature search. Grants that need attribution stay
CLI-only until authentication exists.

## API surface

| Route | Meaning |
| --- | --- |
| `GET /api/health` | liveness and version |
| `GET /api/runs` | run listing with goal and state |
| `POST /api/runs` | start a run: `{goal, mode?, ranking?, …}` |
| `GET /api/runs/{id}` | complete run snapshot (view model) |
| `POST /api/runs/{id}/stop` | durable stop request |
| `GET /api/runs/{id}/events` | SSE stream of snapshots |
| `GET /api/runs/{id}/report` | exported `REPORT.md` + `manifest.json` |
| `GET /api/runs/{id}/sources` | the run's recorded sources |
| `GET /api/runs/{id}/passages?q=…` | local FTS5 passage search |
| `GET /api/library` | all sources across runs |

`POST /api/runs` accepts `mode` (`task` default, `campaign`) and `ranking`
(`simple` default, `tournament`). Both ranking options keep the Elo
tournament; the stored difference is the §7 debate turn cap (1 versus 5),
which is what "single-turn comparison" versus "multi-turn scientific
debate" means in this engine. Request bodies are capped at 64 KiB and
validated; every response carries an `x-request-id`.

## Local development

Two services, two terminals:

```sh
# 1. The API (from the project directory you want to work in):
cargo run -- serve                  # binds 127.0.0.1:7913

# 2. The frontend:
cd web
pnpm install
pnpm dev                            # binds localhost:3000, proxies /api
```

Open `http://localhost:3000`. A model provider is configured exactly as
for the CLI (`URUK_PROVIDER_URL`, `URUK_MODEL`, `URUK_API_KEY`, read from
the project's `.env`); without one, runs complete offline with a stand-in
that states plainly that nothing was analysed.

Environment variables for the frontend:

| Variable | Default | Meaning |
| --- | --- | --- |
| `URUK_API_URL` | `http://127.0.0.1:7913` | where Next (server side and the dev rewrite) reaches the Rust API |

## Production, same origin

Build and run both services, then put a reverse proxy in front so the
browser sees one origin:

```sh
cargo build --release && ./target/release/uruk serve
cd web && pnpm build && pnpm start -p 3000
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

Keep the proxy itself on loopback, or put real authentication in front of
it; see the warning at the top. This repository documents a verified local
setup only; nothing here has been deployed.

## Testing

- Rust: `cargo test --locked` includes `tests/web_api.rs`, which drives
  the router through `tower::ServiceExt::oneshot` against temporary
  SQLite stores and the mock provider: view-model projection, validation,
  stable error bodies, stop, report, passages, body caps, request IDs,
  and SSE (initial snapshot, terminal settle). Everything is offline.
- Frontend: `cd web && pnpm check` runs ESLint, `tsc --noEmit`, Vitest
  (runtime parsers, formatting, the SSE reducer, form validation, agent
  cards, the live run view with a scripted EventSource), and the
  production build.
