# Uruk

An autonomous research engine, implemented entirely in Rust. It generates,
reviews, ranks, and tests hypotheses, and reports a finding as supported only
when evidence beyond agent opinion backs it. Researchers set the goal, approve
side effects, and can steer at any point; they are not needed to do the work.

Uruk is the system specified in [`docs/SPEC.md`](docs/SPEC.md): a Supervisor plus six
specialized roles (Generation, Reflection, Ranking, Evolution, Proximity,
Meta-review) over durable, immutable research records. The research
architecture follows Gottweis et al., *Towards an AI co-scientist* (2025) and
*Accelerating scientific discovery with Co-Scientist* (2026); the Rust-native
runtime, evidence provenance model, and task mode are Uruk extensions.

The name is the city of Gilgamesh: where writing and record-keeping began, and
where, the epic says, the Seven Sages laid the foundations.

## Status

The v1 delivery order in SPEC §13 is implemented and exercised end to end by
the test suite: records and SQLite persistence, the Tokio runtime with the
§9.2 recovery and §9.3 cancellation guarantees, source ingestion from local
files, PDFs, and URLs, task and campaign modes, all six roles scheduled by the
Supervisor, the eight adapted prompt templates, approval-gated contained
execution with reproducibility records, mid-run goal revision, an
OpenAI-compatible HTTP provider, the web API, and the report exporter.
The product surface is web-only: the binary's single command is
`uruk serve`, and some engine capabilities are not yet reachable from the
web surface (see "What a web run can do" below).

Built on Rust 1.98 (edition 2024) with current crates: `reqwest` 0.13 over
rustls for the model provider and retrieval, `pdf_oxide` for PDF text, and
`dom_smoothie` (a readability port) for web pages.

## Web console

The browser is Uruk's only frontend. `uruk serve` — the binary's single
command — starts a local JSON/SSE API on `127.0.0.1:7913` (axum), and
`web/` holds the Next.js frontend for it: full run specification (goal,
rubric, budgets, source
URLs, file uploads, explicit network and literature-search grants, and a
dry-run plan preview), a live agent-topology view, reports, and the
source library. Identity is one anonymous persistent browser cookie — no
accounts or login: each browser sees and controls only the runs it started,
ownerless runs from older builds are never exposed over the web, and
clearing site data permanently loses access. The cookie is a bearer token, so keep
the API on loopback or behind a TLS proxy with `URUK_COOKIE_SECURE=true`.
Setup, architecture, the cookie contract, and the reverse-proxy recipe
are in [docs/WEB.md](docs/WEB.md).

## Build and test

```sh
cargo build --release
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked          # no network, no API key, no paid LLM
cd web && bun run check      # frontend: lint + typecheck + vitest + build
```

The toolchain is pinned in `rust-toolchain.toml`. Tests that need OS
containment skip themselves on hosts without `sandbox-exec` (macOS) or
`bwrap` (Linux). Network tests use a local listener, never the internet.

## Configure a model

Any OpenAI-compatible `/chat/completions` endpoint works. Put the settings in
a `.env` file in the project directory (it is git-ignored) and `uruk serve`
picks them up at startup:

```sh
# .env
URUK_PROVIDER_URL=http://localhost:11434/v1   # Ollama; or https://api.openai.com/v1
URUK_MODEL=qwen3.5:9b
URUK_API_KEY=...                              # optional for local servers
URUK_REASONING_EFFORT=none                    # optional; stops reasoning models thinking at length
URUK_PROVIDER_RPM=14                          # optional; paces request starts (0 disables)
```

A variable exported in the shell overrides the file.

`URUK_PROVIDER_RPM` spaces out request *starts* (one per `60/RPM` seconds,
shared across all workers in the process) without limiting how many requests
may be in flight at once; retries are paced the same way, and a 429
`Retry-After` is honored on top. SwissAI endpoints (any `swissai` host, e.g.
`https://api.swissai.cscs.ch/v1`) default to the platform's published limit
of 14 requests/minute; every other endpoint is unpaced unless the variable
is set. `URUK_PROVIDER_RPM=0` switches pacing off, including that default.
One endpoint gets exactly one schedule per process: if differently-configured
handles ever target the same endpoint, the strictest rate binds them all.

Without these, a run proceeds offline against a stand-in that performs no
analysis. The report says so plainly rather than fabricating results.

## Use

Two processes, one browser:

```sh
uruk serve                  # the only command: the web API on 127.0.0.1:7913
cd web && bun run dev       # the web app on localhost:3000 (proxies /api)
```

Open `http://localhost:3000`: specify a goal, deliverables, preferences,
attributes, constraints, and budgets; watch the agent topology live; read
the report; search the run's passages. The same operations are plain JSON
if an outer agent should drive Uruk as a tool (full surface and the cookie
contract in [docs/WEB.md](docs/WEB.md)):

```sh
curl -c jar -b jar -X POST localhost:7913/api/runs \
     -H 'content-type: application/json' \
     -d '{"goal": "Find competing explanations for this anomaly",
          "mode": "campaign", "ranking": "tournament"}'
curl -b jar localhost:7913/api/runs                      # this browser's runs
curl -b jar localhost:7913/api/runs/<id>                 # live snapshot
curl -b jar -X POST localhost:7913/api/runs/<id>/stop    # durable stop request
curl -b jar localhost:7913/api/runs/<id>/report          # REPORT.md + manifest
curl -b jar 'localhost:7913/api/runs/<id>/passages?q=catalyst+degradation'
```

Errors carry a stable `kind`
(`validation`, `permission`, `budget`, `not_found`, `provider`, `search`,
`cancelled`, …)
to branch on without parsing prose.

### What a web run can do

A web run analyses its goal with the configured model and reports
evidence-backed findings; it grants the engine nothing with side effects.
No file inputs, no network retrieval, no literature search, and no tool
execution: an anonymous cookie identifies a browser, not a person who can
be held to a grant.

**Computational execution is unavailable in the web-only product.** The
engine runs a program only under an approval bound to the exact payload
hash (SPEC §12), and the web console has no UI yet that shows that exact
payload and records the researcher's approval of it. Until an
exact-payload approval UI exists, no execution request can be granted —
items that would need an experiment stay below `Supported` for lack of
empirical evidence rather than anything executing silently. Ingestion,
federated literature search,
and contained execution remain implemented and tested in the engine
(`src/tools`, `src/search`, `src/runtime`) and return to the surface once
the API can attribute an exact-payload approval to someone.

## Outputs

```
<project>/runs/<run-id>/
  manifest.json     identity, versions, providers, permissions, usage, artifact hashes
  REPORT.md         the deliverable with claim provenance, or a labelled partial result
  REPORT.pdf        the same report rendered deterministically to PDF, no model call
  research.jsonl    items, evidence, reviews, decisions, experiments, lineage
  sources.jsonl     source provenance, citation locators, and search records
  works.jsonl       every work found by literature search, with RRF score and per-connector ranks
  tournament.jsonl  matches and ratings (campaigns with comparisons)
  artifacts/        per-task directories: transcripts, logs, execution outputs
```

Reports are derived from persisted records and need no model call, so budget
exhaustion, a stop request, or a crash still yields a labelled partial report.
The final deliverable draws on a reserved slice of the call budget; when it
cannot run, the report says why.

## What the design protects

- **Item content is immutable.** Revision and evolution create linked children;
  a child inherits neither its parent's rating nor its support.
- **`Supported` and `Refuted` require claim-relevant empirical evidence** linked
  to the item being assessed. Agent critique, however unanimous, cannot promote
  a claim. Enforced in the store, so no code path bypasses it.
- **Evidence is consumed, never assumed.** An approved execution records an
  experiment and evidence linked as `context`; a later review interprets it,
  and only then can the assessment move.
- **Elo is relative prioritisation within one rubric cohort**, never a
  probability. A rubric revision starts a fresh cohort; new contradicting
  evidence marks rankings stale and schedules a refresh comparison.
- **Judges see substantive reviews, never numeric scores, Elo, or credits**, and
  presentation order is shuffled and mapped back to stable IDs.
- **Debates stop at their turn cap** (10 max, 5 by default) without fabricating
  a winner.
- **Safety is checked separately** on the goal, each generated or evolved
  candidate, and the directions in each research overview.
- **Crash safety.** A task's records, outcome, and budget settlement commit in
  one transaction, so a kill before commit leaves nothing to duplicate and a
  replay after commit changes nothing. Interrupted external effects become
  `uncertain` rather than being retried blindly. Approvals bind to an exact
  payload hash and survive restart. Writes use `BEGIN IMMEDIATE`, so a
  concurrent control write (a stop request) during a run queues instead
  of failing.
- **Failed work is charged.** Every attempted model call is settled, retries
  are bounded, and a cancelled task keeps its labelled partial artifacts.
- **Execution is contained or unavailable.** Approved programs run under macOS
  `sandbox-exec` or Linux bubblewrap with no network and writes confined to a
  task workspace; inputs must lie within the run's readable paths.
- **Sources say what was read.** A PDF is full text only when its pages
  yielded text; a scanned PDF, a failed fetch, or a URL supplied without the
  network permission is recorded as such, with the reason, and the report
  repeats it. Retrieved pages and PDFs are untrusted data, fenced in prompts.
- **Search disclosure is explicit opt-in and auditable.** Only the
  per-run search grants (`search:openalex`, `search:crossref`,
  `search:arxiv` on top of the network permission) enable connectors; the
  network permission alone keeps URL-fetch-only behavior and sends zero
  connector traffic. Web runs today grant neither (see "What a web run
  can do"). With search enabled, query text, year filters, and the configured
  contact email (`URUK_CONTACT_EMAIL`, polite pools) are transmitted to the
  enabled operators (OurResearch, Crossref, arXiv/Cornell) — never source
  contents, never the goal record. Every transmitted query is persisted
  verbatim in a `SearchRecord` and printed in REPORT.md. Acquisition fetches
  only URLs a connector designates as legal open-access locations; paywalled
  works are recorded as abstract-only or unavailable, never bypassed.

## Layout

```
src/records/   data contracts (SPEC §4)
src/store/     SQLite persistence, the single controlled write path, RecordBatch (§9.2)
src/agents/    research policy: the roles, safety, typed outputs, the Supervisor (§5, §6)
src/prompts/   versioned templates with provenance (§5.1, §15)
src/provider/  OpenAI-shaped wire types, the HTTP adapter, and the mock
src/runtime/   Tokio scheduler, limits, executor (§6, §9)
src/search/    literature search: chunking, FTS5 passages, connectors, dedup, RRF
src/tools/     ingestion, PDF and HTML extraction, contained execution (§8, §9.3)
src/report/    deterministic export (§11)
prompts/       the template assets
```

## Literature search

Every ingested source with a text artifact is chunked into passages and
indexed in SQLite FTS5 inside `.uruk/state.sqlite`; prompts ground in ranked
passages, and `GET /api/runs/{id}/passages` queries the index offline —
this needs no network at all. With the network and search permissions
granted (not yet possible from the web surface), discovery federates
OpenAlex, Crossref, and arXiv (freely accessible public APIs — Uruk runs
none of their code), deduplicates by DOI/arXiv/PMID/title fingerprint, fuses
rankings with Reciprocal Rank Fusion, and acquires open-access full text
through the ordinary ingestion pipeline. Resolution is local, from the
discovery data already in hand: the arXiv PDF when the work has an arXiv id,
else OpenAlex's designated `best_oa_location` (PDF, then landing page), else
its `oa_url` — no resolver API is called. Every query, connector, result
count, acquisition, and failure lands in the report's "Search coverage"
section; each work's per-connector ranks and raw-response hashes ride in its
own record, and `works.jsonl` lists everything found, not just everything
read.

## Not implemented

- Web surfaces for granting file inputs, network retrieval, literature
  search, or computational execution. All of them wait on the same
  prerequisite: a UI that can show the exact payload being approved and
  attribute that approval (see "What a web run can do").
- Unpaywall open-access resolution; resolution currently uses only the OA
  locations the discovery connectors themselves designate.
- PubMed/PMC (NCBI E-utilities) connector and JATS full-text extraction;
  biomedical coverage currently comes via OpenAlex.
- OCR for scanned PDFs; they are recorded as metadata-only.
- The research market (SPEC §7.1) is post-v1, opt-in, and deliberately not
  scaffolded.
