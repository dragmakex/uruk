//! Literature search: local passage grounding, federated discovery,
//! open-access acquisition, and the opt-in/zero-disclosure semantics.
//!
//! Everything runs offline: an inline TCP fixture server plays every
//! connector (routed by path) and serves the full text; the MockProvider is
//! anchored on the `search.plan_queries` template. The CLI opt-in contract
//! (`--search` requires `--allow-network`; `--allow-network` alone opens
//! zero connector connections) is exercised against the real binary.

mod common;

use common::*;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::report;
use uruk::runtime::{Scheduler, SchedulerConfig};
use uruk::tools::ingest_input;

/// Serializes tests that point the connector base URLs at their own fixture
/// server through the process environment.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn search_env(base: &str) -> MutexGuard<'static, ()> {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: serialized by ENV_LOCK; only these tests touch these variables.
    unsafe {
        std::env::set_var("URUK_OPENALEX_BASE", format!("{base}/openalex"));
        std::env::set_var("URUK_CROSSREF_BASE", format!("{base}/crossref"));
        std::env::set_var("URUK_ARXIV_BASE", format!("{base}/arxiv"));
        std::env::set_var("URUK_CONTACT_EMAIL", "uruk-tests@example.org");
    }
    guard
}

struct FixtureServer {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
}

impl FixtureServer {
    fn hit_log(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }

    /// Requests that reached any connector route (vs. plain URL fetches).
    fn connector_hits(&self) -> Vec<String> {
        self.hit_log()
            .into_iter()
            .filter(|p| {
                p.starts_with("/openalex") || p.starts_with("/crossref") || p.starts_with("/arxiv")
            })
            .collect()
    }
}

fn fixture_file(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/search")
            .join(name),
    )
    .unwrap()
}

fn repo_pdf() -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/pre_coscientist.pdf"))
        .unwrap()
}

/// Build the connector fixture server: bind, then rewrite fixture URLs onto
/// the bound base so full-text links point back at this server and the test
/// stays entirely offline. Routes by longest path prefix; every request is
/// logged for the zero-disclosure assertions.
async fn spawn_connector_server() -> FixtureServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());

    let rewrite = |s: String| -> Vec<u8> {
        s.replace("http://arxiv.org/pdf", &format!("{base}/pdf"))
            .replace("https://arxiv.org/pdf", &format!("{base}/pdf"))
            .replace("https://example.org/oa", &format!("{base}/oa"))
            .into_bytes()
    };

    let routes: Vec<(String, &'static str, Vec<u8>)> = vec![
        (
            "/openalex/works".to_string(),
            "application/json",
            rewrite(fixture_file("openalex_works.json")),
        ),
        (
            "/crossref/works".to_string(),
            "application/json",
            rewrite(fixture_file("crossref_works.json")),
        ),
        (
            "/arxiv/api/query".to_string(),
            "application/atom+xml",
            rewrite(fixture_file("arxiv_atom.xml")),
        ),
        ("/pdf/".to_string(), "application/pdf", repo_pdf()),
        ("/oa/".to_string(), "application/pdf", repo_pdf()),
        (
            "/badpdf/".to_string(),
            "application/pdf",
            b"%PDF-1.7 these bytes do not parse as a PDF".to_vec(),
        ),
        (
            "/article".to_string(),
            "text/html",
            b"<html><head><title>Supplied article</title></head><body><article>\
              <h1>Supplied article</h1><p>A supplied URL fetched without any \
              connector: the twenty-three percent increase was observed in the \
              treated group across forty samples.</p><p>Second paragraph keeps \
              the readability heuristic on the article body.</p></article>\
              </body></html>"
                .to_vec(),
        ),
    ];

    let hits = Arc::new(Mutex::new(Vec::new()));
    let log = hits.clone();
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let routes = routes.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut total = 0;
                loop {
                    let Ok(n) = sock.read(&mut buf[total..]).await else {
                        return;
                    };
                    total += n;
                    if n == 0 || buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf[..total]);
                let path = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                log.lock().unwrap().push(path.clone());

                let best = routes
                    .iter()
                    .filter(|(prefix, _, _)| path.starts_with(prefix.as_str()))
                    .max_by_key(|(prefix, _, _)| prefix.len());
                let (status, content_type, body) = match best {
                    Some((_, ct, body)) => (200, *ct, body.clone()),
                    None => (404, "text/plain", b"not found".to_vec()),
                };
                let response_head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(response_head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });

    FixtureServer { base, hits }
}

const PLANNED_QUERY: &str = "catalyst degradation thermal cycling";

fn query_plan_json() -> String {
    serde_json::json!({
        "queries": [{
            "text": PLANNED_QUERY,
            "rationale": "direct formulation of the goal",
            "year_from": null,
            "year_to": null
        }]
    })
    .to_string()
}

/// What `uruk run --allow-network --search` produces (network plus the
/// three connector allowlist entries).
fn search_permissions() -> Permissions {
    Permissions {
        network: true,
        allowed_tools: uruk::search::DEFAULT_CONNECTOR_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        disclose_to_provider: true,
        ..Default::default()
    }
}

/// Acceptance 2, 3 (partial), 5 (replay), 7: a task-mode run with fixture
/// connectors produces verbatim SearchRecords, a deduplicated and fused
/// works table (hits in each work's body), full-text and abstract-only
/// acquisitions, the coverage report, and no re-querying after completion.
#[tokio::test(flavor = "multi_thread")]
// The env guard intentionally spans the whole test: it serializes the
// process-wide base-URL overrides across tests, not an async resource.
#[expect(clippy::await_holding_lock)]
async fn federated_discovery_and_acquisition_end_to_end() {
    let server = spawn_connector_server().await;
    let _env = search_env(&server.base);

    let provider = MockProvider::new()
        .rule(anchor::SEARCH_PLAN_QUERIES, query_plan_json())
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "fine", false));

    let f = fixture_with(Mode::Task, provider, |g| {
        g.permissions = search_permissions();
    })
    .await;

    let scheduler = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    );
    let summary = scheduler.run(&f.run_id).await.unwrap();
    assert_eq!(summary.state, RunState::Completed, "{summary:?}");

    // --- SearchRecords: one per (connector × query), queries verbatim ---
    let records = f.store.list_search_records(&f.run_id).await.unwrap();
    assert_eq!(records.len(), 3, "{records:?}");
    for record in &records {
        assert_eq!(record.query, PLANNED_QUERY, "query persisted verbatim");
        assert!(record.connector_errors.is_empty(), "{record:?}");
    }
    let connectors: std::collections::BTreeSet<&str> =
        records.iter().map(|r| r.tool.as_str()).collect();
    assert_eq!(
        connectors.into_iter().collect::<Vec<_>>(),
        vec!["arxiv", "crossref", "openalex"]
    );
    // results_found is the connector-reported total, not the page size: the
    // crossref fixture reports 40 matches while returning 3 items.
    let found_of = |tool: &str| {
        records
            .iter()
            .find(|r| r.tool == tool)
            .unwrap()
            .results_found
    };
    assert_eq!(found_of("openalex"), 3);
    assert_eq!(found_of("crossref"), 40);
    assert_eq!(found_of("arxiv"), 2);

    // --- Works: deduplicated, RRF-fused, hand-checked order ---
    // Lists: openalex [A, B, E]; crossref [A, C, D]; arxiv [B, A].
    // A = 2/61 + 1/62, B = 1/61 + 1/62, C = 1/62, E = D = 1/63; the E/D tie
    // breaks lexicographically on work_key.
    let works = f.store.list_works(&f.run_id).await.unwrap();
    let keys: Vec<&str> = works.iter().map(|w| w.work.work_key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "doi:10.1234/catalyst.2024.001",   // A
            "arxiv:2403.01234",                // B
            "doi:10.5555/other.2023.9",        // C
            "doi:10.7777/openalexonly.2024.3", // E
            "doi:10.9999/paywalled.2022.7",    // D
        ],
        "RRF order must match the hand computation"
    );

    // Same DOI from three connectors: one work, three hits in its body.
    let top = &works[0];
    assert_eq!(top.work.hits.len(), 3, "{:?}", top.work.hits);
    let hit_connectors: std::collections::BTreeSet<&str> =
        top.work.hits.iter().map(|h| h.connector.as_str()).collect();
    assert_eq!(
        hit_connectors.into_iter().collect::<Vec<_>>(),
        vec!["arxiv", "crossref", "openalex"]
    );
    let record_ids: std::collections::BTreeSet<&str> =
        records.iter().map(|r| r.id.as_str()).collect();
    for hit in &top.work.hits {
        assert!(
            record_ids.contains(hit.search_id.as_str()),
            "every body hit points at a persisted SearchRecord"
        );
    }

    // --- Acquisition ---
    let sources = f.store.list_sources(&f.run_id).await.unwrap();
    let source_of = |key: &str| {
        let w = works
            .iter()
            .find(|w| w.work.work_key.as_str() == key)
            .unwrap();
        let id = w
            .source_id
            .as_ref()
            .unwrap_or_else(|| panic!("{key} not acquired"));
        sources.iter().find(|s| &s.id == id).unwrap()
    };

    // Full text via the arXiv PDF route.
    let full = source_of("doi:10.1234/catalyst.2024.001");
    assert_eq!(full.access, AccessLevel::FullText, "{full:?}");
    assert_eq!(
        full.identifier.as_deref(),
        Some("doi:10.1234/catalyst.2024.001")
    );
    assert_eq!(source_of("arxiv:2403.01234").access, AccessLevel::FullText);

    // Full text via OpenAlex best_oa_location (local resolution, no
    // resolver API call).
    let via_openalex = source_of("doi:10.7777/openalexonly.2024.3");
    assert_eq!(
        via_openalex.access,
        AccessLevel::FullText,
        "{via_openalex:?}"
    );

    // Abstract fallback for the work with no OA location but an abstract.
    let fallback = source_of("doi:10.5555/other.2023.9");
    assert_eq!(fallback.access, AccessLevel::AbstractOnly, "{fallback:?}");
    assert!(
        !fallback.access.permits_fulltext_claim(),
        "abstract-only must never permit a full-text claim"
    );
    assert!(
        fallback
            .access_limitations
            .as_deref()
            .unwrap_or("")
            .contains("not openly accessible")
    );

    // No location and no abstract: recorded unavailable, no source.
    let paywalled = works
        .iter()
        .find(|w| w.work.work_key.as_str() == "doi:10.9999/paywalled.2022.7")
        .unwrap();
    assert!(paywalled.source_id.is_none());
    let unavailable: Vec<String> = records.iter().flat_map(|r| r.unavailable.clone()).collect();
    assert_eq!(unavailable.len(), 1, "{unavailable:?}");
    assert!(
        unavailable[0].contains("10.9999/paywalled.2022.7")
            && unavailable[0].contains("no open-access location"),
        "{unavailable:?}"
    );

    // Retrieved sources are recorded on the originating search records.
    let retrieved: Vec<SourceId> = f
        .store
        .list_search_records(&f.run_id)
        .await
        .unwrap()
        .iter()
        .flat_map(|r| r.results_retrieved.clone())
        .collect();
    assert_eq!(retrieved.len(), 4, "{retrieved:?}");

    // No unpaywall and no resolver call: connector traffic is exactly one
    // search per connector; everything else is full-text fetches.
    let connector_paths = server.connector_hits();
    assert_eq!(connector_paths.len(), 3, "{connector_paths:?}");
    assert!(
        server.hit_log().iter().all(|p| !p.contains("unpaywall")),
        "unpaywall is not part of the MVP: {:?}",
        server.hit_log()
    );

    // --- Passages: a hit whose span matches the artifact bytes exactly ---
    let passage_hits = f
        .store
        .search_passages(&f.run_id, "electrode fouling", 5)
        .await
        .unwrap();
    assert!(!passage_hits.is_empty(), "abstract text must be indexed");
    let hit = &passage_hits[0];
    let source = sources.iter().find(|s| s.id == hit.source_id).unwrap();
    let artifact = f
        .store
        .get_artifact(source.text_artifact.as_ref().unwrap())
        .await
        .unwrap();
    let text = tokio::fs::read_to_string(f.store.resolve_path(&artifact.storage_path))
        .await
        .unwrap();
    assert_eq!(&text[hit.byte_start..hit.byte_end], hit.text);

    // --- Report and exports ---
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    assert!(export.files.contains(&"works.jsonl".to_string()));
    let report_text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert!(report_text.contains("### Search coverage"), "{report_text}");
    assert!(
        report_text.contains(PLANNED_QUERY),
        "verbatim queries belong in the report"
    );
    assert!(report_text.contains("abstract-only"));
    assert!(
        report_text.contains("openalex, crossref, arxiv"),
        "{report_text}"
    );
    let manifest: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(export.dir.join("manifest.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["counts"]["works"], 5);
    assert_eq!(manifest["counts"]["searches"], 3);
    assert!(manifest["counts"]["passages"].as_u64().unwrap() > 0);
    let works_jsonl = tokio::fs::read_to_string(export.dir.join("works.jsonl"))
        .await
        .unwrap();
    assert_eq!(works_jsonl.lines().count(), 5);
    assert!(works_jsonl.contains("rrf_score"));
    assert!(
        works_jsonl.contains("\"hits\""),
        "per-connector ranks ride in each work body"
    );

    // Raw connector responses were persisted local-only.
    let artifacts = f.store.list_artifacts(&f.run_id).await.unwrap();
    let raw: Vec<_> = artifacts
        .iter()
        .filter(|a| a.label.as_deref().unwrap_or("").contains("response:"))
        .collect();
    assert_eq!(raw.len(), 3, "one raw body per connector search");
    assert!(raw.iter().all(|a| a.access == AccessClass::LocalOnly));

    // --- Replay: nothing re-queries after completion (acceptance 5) ---
    let hits_after_run = server.hit_log().len();
    report::export_run(&f.store, &f.run_id).await.unwrap();
    let goal = f.store.current_goal(&f.run_id).await.unwrap();
    let plan = f.store.current_plan(&f.run_id).await.unwrap().unwrap();
    let proposals = uruk::agents::supervisor::select_work(
        &f.store,
        &f.run_id,
        &goal,
        &plan,
        &uruk::agents::supervisor::Weights::default(),
        8,
        99,
    )
    .await
    .unwrap();
    assert!(
        !proposals.iter().any(|w| w.role == Role::Search),
        "a completed discovery must not be re-proposed: {proposals:?}"
    );
    assert_eq!(
        server.hit_log().len(),
        hits_after_run,
        "re-export and re-selection must not touch any connector"
    );
}

/// Acceptance 1: a no-network run over a supplied file indexes passages and
/// grounds the generation prompt in them (visible through the MockProvider).
#[tokio::test]
async fn local_passages_ground_prompts_without_network() {
    let marker = "the heat exchanger showed a twelve percent efficiency loss";
    let provider = MockProvider::new()
        .rule(
            anchor::GENERATION_LITERATURE,
            hypothesis_json("Fouling", "fouling explains the efficiency loss"),
        )
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "fine", false));

    let f = fixture_with(Mode::Task, provider, |g| {
        g.question = "propose explanations for the efficiency loss".into();
    })
    .await;

    // Ingest a real file through the pipeline, so indexing runs on ingest.
    let input = f.dir.path().join("log.md");
    tokio::fs::write(
        &input,
        format!(
            "Observation log\n\nDuring the trial, {marker}.\n\nA second paragraph on maintenance."
        ),
    )
    .await
    .unwrap();
    ingest_input(
        &f.store,
        &f.run_id,
        &InputRef {
            locator: input.to_string_lossy().into_owned(),
            note: None,
        },
        &f.goal.permissions,
    )
    .await
    .unwrap();

    assert!(f.store.count_passages(&f.run_id).await.unwrap() > 0);

    // Ranked local retrieval with valid byte spans (the `uruk passages` path).
    let hits = f
        .store
        .search_passages(&f.run_id, "heat exchanger efficiency", 10)
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert!(hits[0].text.contains(marker));
    assert!(hits[0].byte_end > hits[0].byte_start);

    let scheduler = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    );
    let summary = scheduler.run(&f.run_id).await.unwrap();
    assert_eq!(summary.state, RunState::Completed, "{summary:?}");

    // No network: no search task may exist.
    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    assert!(tasks.iter().all(|t| t.role != Role::Search), "{tasks:?}");

    // The generation prompt was passage-grounded, not whole-document.
    let generation_prompt = f
        .provider
        .seen_prompts()
        .into_iter()
        .find(|p| p.contains(anchor::GENERATION_LITERATURE))
        .expect("generation ran");
    assert!(
        generation_prompt.contains("BEGIN PASSAGE"),
        "generation must receive ranked passages"
    );
    assert!(generation_prompt.contains(marker));
}

/// Acceptance 3 at the scheduler level: without the `--search` allowlist
/// entries, a network-permitted run fetches supplied URLs but schedules no
/// search task and opens zero connector connections.
#[tokio::test(flavor = "multi_thread")]
async fn allow_network_without_search_schedules_nothing_and_sends_nothing() {
    let server = spawn_connector_server().await;

    // Network off entirely: connectors allowlisted but nothing may run.
    let provider = MockProvider::new()
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "fine", false));
    let f = fixture_with(Mode::Task, provider, |g| {
        g.permissions.network = false;
        g.permissions.allowed_tools = uruk::search::DEFAULT_CONNECTOR_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
    })
    .await;
    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    assert_eq!(summary.state, RunState::Completed);
    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    assert!(tasks.iter().all(|t| t.role != Role::Search));
    assert!(server.hit_log().is_empty(), "{:?}", server.hit_log());

    // Network on but no --search: a supplied URL is fetched, and no
    // goal-derived query reaches any connector.
    let provider = MockProvider::new()
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "fine", false));
    let f2 = fixture_with(Mode::Task, provider, |g| {
        g.permissions.network = true;
        g.permissions.allowed_tools = vec![]; // no --search
    })
    .await;
    ingest_input(
        &f2.store,
        &f2.run_id,
        &InputRef {
            locator: format!("{}/article", server.base),
            note: None,
        },
        &f2.goal.permissions,
    )
    .await
    .unwrap();

    let summary = Scheduler::new(
        f2.store.clone(),
        f2.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f2.run_id)
    .await
    .unwrap();
    assert_eq!(summary.state, RunState::Completed);
    assert!(
        f2.store
            .list_tasks(&f2.run_id)
            .await
            .unwrap()
            .iter()
            .all(|t| t.role != Role::Search)
    );
    let log = server.hit_log();
    assert_eq!(log, vec!["/article".to_string()], "only the supplied URL");
    assert!(server.connector_hits().is_empty());

    // The supplied article was read and indexed.
    let sources = f2.store.list_sources(&f2.run_id).await.unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].access, AccessLevel::FullText);
    assert!(f2.store.count_passages(&f2.run_id).await.unwrap() > 0);
}

/// Focused CLI contract: `--search` without `--allow-network` is a
/// validation error, reported with the stable `validation` kind, before any
/// run is created.
#[test]
fn cli_search_without_allow_network_is_a_validation_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_uruk"))
        .args([
            "--json",
            "--project",
            dir.path().to_str().unwrap(),
            "run",
            "--goal",
            "what is known about x",
            "--search",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["kind"], "validation", "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("--search requires --allow-network"),
        "{body}"
    );
    assert!(
        !dir.path().join(".uruk").exists(),
        "validation must precede any state creation"
    );

    // --search-connectors without --search is refused the same way.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_uruk"))
        .args([
            "--json",
            "--project",
            dir.path().to_str().unwrap(),
            "run",
            "--goal",
            "what is known about x",
            "--allow-network",
            "--search-connectors",
            "openalex",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["kind"], "validation", "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("requires --search"),
        "{body}"
    );
}

/// Focused CLI contract: `--allow-network` without `--search` runs the real
/// binary end to end, fetches only the supplied URL, and opens zero
/// connector connections even with connector base URLs pointing at a live
/// fixture server.
#[tokio::test(flavor = "multi_thread")]
async fn cli_allow_network_without_search_opens_no_connector_connection() {
    let server = spawn_connector_server().await;
    let dir = tempfile::tempdir().unwrap();

    let out = tokio::task::spawn_blocking({
        let base = server.base.clone();
        let project = dir.path().to_str().unwrap().to_string();
        move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_uruk"))
                // Child-process env only: no global env mutation, and the
                // bases point at the fixture server so any connector call
                // would be visible in its hit log.
                .env("URUK_OPENALEX_BASE", format!("{base}/openalex"))
                .env("URUK_CROSSREF_BASE", format!("{base}/crossref"))
                .env("URUK_ARXIV_BASE", format!("{base}/arxiv"))
                .args([
                    "--json",
                    "--project",
                    &project,
                    "run",
                    "--goal",
                    "summarise the supplied article",
                    "--allow-network",
                    "--input",
                    &format!("{base}/article"),
                ])
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();

    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["run"]["state"], "completed", "{body}");

    assert_eq!(
        server.hit_log(),
        vec!["/article".to_string()],
        "--allow-network alone must fetch only the supplied URL"
    );
    assert!(server.connector_hits().is_empty());
}

/// An acquire task for `work_key`, shaped like the ones the discover task
/// enqueues.
fn acquire_task(f: &Fixture, work_key: &str) -> Task {
    Task {
        id: TaskId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        role: Role::Search,
        strategy: "acquire".into(),
        goal_id: f.goal.id.clone(),
        plan_id: f.plan.id.clone(),
        input_refs: vec![work_key.to_string()],
        input_hash: ContentHash::of_str(work_key),
        prompt_id: None,
        prompt_hash: None,
        model: None,
        provider: None,
        payload: None,
        feedback_id: None,
        depends_on: vec![],
        priority: 1.0,
        priority_rationale: "test acquisition".into(),
        permissions: f.goal.permissions.clone(),
        reservation: CostReservation {
            model_calls: 0,
            tokens: 0,
            tool_executions: 1,
        },
        state: TaskState::Pending,
        attempts: 0,
        max_attempts: 1,
        seed: 7,
        output_refs: vec![],
        actual_cost: CostActual::default(),
        error: None,
        created_at: time::OffsetDateTime::now_utc(),
        started_at: None,
        finished_at: None,
    }
}

/// Regression: an OA location whose fetch yields no readable text (here a
/// PDF that does not parse) must fall back to the abstract-only source, not
/// be linked as a full-text acquisition and not fail the task.
#[tokio::test(flavor = "multi_thread")]
async fn acquire_falls_back_to_abstract_when_fulltext_is_unreadable() {
    let server = spawn_connector_server().await;
    let f = fixture_with(Mode::Task, MockProvider::new(), |g| {
        g.permissions = search_permissions();
    })
    .await;

    let mut work = uruk::search::WorkRecord {
        title: "Unreadable full text".into(),
        doi: Some("10.1/unreadable".into()),
        oa_pdf_url: Some(format!("{}/badpdf/x.pdf", server.base)),
        abstract_text: Some("An abstract that is perfectly readable.".into()),
        year: Some(2024),
        ..Default::default()
    };
    work.work_key = uruk::search::WorkKey("doi:10.1/unreadable".into());
    f.store.insert_work(&f.run_id, &work, 0.02).await.unwrap();

    let task = acquire_task(&f, "doi:10.1/unreadable");
    let mut batch = uruk::store::RecordBatch::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    uruk::agents::search::run_search(&context(&f, cancel), &task, &mut batch)
        .await
        .unwrap();

    let stored = f
        .store
        .get_work(&f.run_id, "doi:10.1/unreadable")
        .await
        .unwrap();
    let source_id = stored.source_id.expect("the fallback must link a source");
    let sources = f.store.list_sources(&f.run_id).await.unwrap();
    let source = sources.iter().find(|s| s.id == source_id).unwrap();
    assert_eq!(source.access, AccessLevel::AbstractOnly, "{source:?}");
    assert!(
        source.text_artifact.is_some(),
        "the abstract is indexed like any text-bearing source"
    );
}

/// Regression: a connector response without a Content-Length header is
/// capped while streaming, not buffered unboundedly and checked after.
#[tokio::test(flavor = "multi_thread")]
async fn connector_responses_without_content_length_are_capped_while_streaming() {
    // Stream well past the metadata cap with no Content-Length; the body is
    // delimited by connection close, so only the client can stop the read.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let _ = sock
            .write_all(b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n")
            .await;
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..(9 * 16) {
            // ~9 MiB total
            if sock.write_all(&chunk).await.is_err() {
                return; // the client hung up at the cap, as intended
            }
        }
        let _ = sock.shutdown().await;
    });

    let cx = uruk::search::ConnectorCx::new(None).unwrap();
    let err = cx
        .get_recorded(
            "openalex",
            &format!("http://{addr}/huge"),
            "application/json",
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("metadata cap"),
        "cap must be enforced while streaming: {err}"
    );
    assert!(
        cx.take_raw().is_empty(),
        "no raw body is recorded for a failed read"
    );
}

/// Acceptance 4: a review citing an abstract-only source records the
/// limitation, so a full-text claim can never ride on an abstract.
#[tokio::test]
async fn abstract_only_citations_carry_the_limitation() {
    use uruk::agents::reflection;

    let f = fixture(Mode::Task, MockProvider::new()).await;

    // An abstract-only source with a text artifact (like a fallback source).
    let text = "Electrode fouling abstract only.";
    let artifact_path = f.dir.path().join("abs.txt");
    tokio::fs::write(&artifact_path, text).await.unwrap();
    let artifact = Artifact {
        id: ArtifactId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        media_type: "text/plain".into(),
        content_hash: ContentHash::of_str(text),
        size_bytes: text.len() as u64,
        storage_path: artifact_path.to_string_lossy().into_owned(),
        produced_by: None,
        access: AccessClass::Open,
        label: None,
        supersedes: None,
        superseded_reason: None,
        created_at: time::OffsetDateTime::now_utc(),
    };
    f.store.insert_artifact(&artifact).await.unwrap();
    let source = Source {
        id: SourceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        origin: Origin::Supplied,
        title: Some("Closed study".into()),
        authors: Some("Vaughan".into()),
        date: Some("2023".into()),
        identifier: Some("doi:10.5555/other.2023.9".into()),
        retrieved_at: time::OffsetDateTime::now_utc(),
        content_hash: ContentHash::of_str(text),
        access: AccessLevel::AbstractOnly,
        access_limitations: Some("full text not openly accessible".into()),
        text_artifact: Some(artifact.id),
    };
    f.store.insert_source(&source).await.unwrap();

    let rules = MockProvider::new().rule(
        anchor::REFLECTION_FULL,
        full_review_json(source.id.as_str(), "supports"),
    );
    let f = Fixture {
        provider: std::sync::Arc::new(rules),
        ..f
    };

    let item = seed_item(&f, "fouling hypothesis").await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let reviewed = reflection::review(&context(&f, cancel), &item, ReviewStrategy::Full, None)
        .await
        .unwrap();

    assert_eq!(reviewed.new_evidence.len(), 1);
    let limitation = reviewed.new_evidence[0]
        .limitations
        .clone()
        .unwrap_or_default();
    assert!(
        limitation.contains("only the abstract was accessible"),
        "{limitation}"
    );
    assert!(
        !source.access.permits_fulltext_claim(),
        "abstract-only never permits a full-text claim"
    );
}
