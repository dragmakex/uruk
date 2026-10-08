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
OpenAI-compatible HTTP provider, and the CLI and report exporter.

Built on Rust 1.98 (edition 2024) with current crates: `reqwest` 0.13 over
rustls for the model provider and retrieval, `pdf_oxide` for PDF text, and
`dom_smoothie` (a readability port) for web pages.

## Web console

`uruk serve` starts a local JSON/SSE API on `127.0.0.1:7913` (axum, behind
the on-by-default `web` cargo feature), and `web/` holds a separate Next.js
frontend for it: run specification, a live agent-topology view, reports,
and the source library. Identity is one anonymous persistent browser
cookie — no accounts or login: each browser sees and controls only the
runs it started, CLI runs are never exposed over the web, and clearing
site data permanently loses access. The cookie is a bearer token, so keep
the API on loopback or behind a TLS proxy with `URUK_COOKIE_SECURE=true`.
Setup, architecture, the cookie contract, and the reverse-proxy recipe
are in [docs/WEB.md](docs/WEB.md).

## Build and test

```sh
cargo build --release
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked          # no network, no API key, no paid LLM
```

The toolchain is pinned in `rust-toolchain.toml`. Tests that need OS
containment skip themselves on hosts without `sandbox-exec` (macOS) or
`bwrap` (Linux). Network tests use a local listener, never the internet.

## Configure a model

Any OpenAI-compatible `/chat/completions` endpoint works. Put the settings in
a `.env` file in the project directory (it is git-ignored) and every `uruk`
command picks them up:

```sh
# .env
URUK_PROVIDER_URL=http://localhost:11434/v1   # Ollama; or https://api.openai.com/v1
URUK_MODEL=qwen3.5:9b
URUK_API_KEY=...                              # optional for local servers
URUK_REASONING_EFFORT=none                    # optional; stops reasoning models thinking at length
```

A variable exported in the shell overrides the file.

Without these, a run proceeds offline against a stand-in that performs no
analysis. The report says so plainly rather than fabricating results.

## Use

```sh
# A bounded task over local papers, PDFs included.
uruk run --goal "Compare these papers and explain where they disagree" \
           --mode task --input a.pdf --input b.md --deliverable "cited comparison"

# Sources by URL need the network permission; each fetch is hashed and its
# access level recorded (article text, PDF text, or unavailable with the reason).
uruk run --goal "Summarise the reported effect sizes" --allow-network \
           --input https://doi.org/10.1000/example --input https://example.org/paper.pdf

# An open campaign.
uruk run --goal "Find competing explanations for this anomaly" \
           --mode campaign --max-iterations 10

# Autonomous literature review is an explicit opt-in: --search (requires
# --allow-network) formulates queries, searches OpenAlex/Crossref/arXiv,
# acquires open-access full text, and grounds the synthesis in ranked
# passages. --allow-network alone keeps URL-fetch-only behavior and sends
# zero connector traffic; --search-connectors narrows the three connectors.
uruk run --goal "What is known about catalyst degradation?" --mode task \
           --allow-network --search [--search-connectors openalex,arxiv] [--max-acquisitions 8]
uruk passages --run-id <id> --query "catalyst degradation"   # offline FTS5 lookup

# Contained execution, gated by approval.
uruk run --goal "Assess whether the mean exceeds zero" --input data.csv \
           --allow-execute --tool sh
uruk status   --run-id <id>              # shows the pending request
uruk approve  --run-id <id> --request-id <id> [--payload-hash <hash>]
uruk resume   --run-id <id>

uruk feedback --run-id <id> --file notes.md [--as-item] [--item <item-id>]
uruk revise   --run-id <id> [--goal <text>] [--attribute <axis>]... [--constraint <c>]...
                [--exclude <x>]... [--assume <a>]... [--max-model-calls N] [--reason <why>]
uruk deny     --run-id <id> --request-id <id>
uruk stop     --run-id <id>              # observed by the running scheduler
uruk report   --run-id <id>
uruk prompts                             # templates and their source figures
```

Every command accepts `--json` for machine consumption, so an outer agent can
drive Uruk as a tool. Errors carry a stable `kind`
(`validation`, `permission`, `budget`, `not_found`, `provider`, `search`,
`cancelled`, …)
to branch on without parsing prose.

Capabilities are off by default. `--allow-network`, `--allow-execute`, and
`--tool <name>` grant them explicitly; permissions are enforced at execution
boundaries, not by prompt text. `--allow-execute` is refused on a host without
OS-level containment.

`uruk revise` records a new goal revision. In-flight results keep their
original context; pending approvals are invalidated; a changed rubric starts a
fresh Elo cohort; a finished run is reopened so `uruk resume` continues under
the new revision.

## Outputs

```
<project>/runs/<run-id>/
  manifest.json     identity, versions, providers, permissions, usage, artifact hashes
  REPORT.md         the deliverable with claim provenance, or a labelled partial result
  REPORT.pdf        the same report rendered deterministically to PDF, no model call
  research.jsonl    items, evidence, reviews, decisions, experiments, lineage
  sources.jsonl     source provenance, citation locators, and search records
  works.jsonl       every work found by --search, with RRF score and per-connector ranks
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
  `uruk approve` during a run queues instead of failing.
- **Failed work is charged.** Every attempted model call is settled, retries
  are bounded, and a cancelled task keeps its labelled partial artifacts.
- **Execution is contained or unavailable.** Approved programs run under macOS
  `sandbox-exec` or Linux bubblewrap with no network and writes confined to a
  task workspace; inputs must lie within the run's readable paths.
- **Sources say what was read.** A PDF is full text only when its pages
  yielded text; a scanned PDF, a failed fetch, or a URL supplied without the
  network permission is recorded as such, with the reason, and the report
  repeats it. Retrieved pages and PDFs are untrusted data, fenced in prompts.
- **Search disclosure is explicit opt-in and auditable.** Only `--search`
  (which requires `--allow-network`) enables connectors; `--allow-network`
  alone keeps today's URL-fetch-only behavior and sends zero connector
  traffic. With search enabled, query text, year filters, and the configured
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
passages, and `uruk passages` queries the index offline — this needs no
network at all. With `--allow-network --search`, discovery federates
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

- Unpaywall open-access resolution; resolution currently uses only the OA
  locations the discovery connectors themselves designate.
- PubMed/PMC (NCBI E-utilities) connector and JATS full-text extraction;
  biomedical coverage currently comes via OpenAlex.
- OCR for scanned PDFs; they are recorded as metadata-only.
- The research market (SPEC §7.1) is post-v1, opt-in, and deliberately not
  scaffolded.
