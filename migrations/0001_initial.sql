-- Uruk durable state (SPEC §9.2). One SQLite database per project;
-- the persisted task set is the queue's source of truth.

PRAGMA foreign_keys = ON;

CREATE TABLE projects (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE TABLE runs (
    id              TEXT PRIMARY KEY,
    schema_version  INTEGER NOT NULL,
    project_id      TEXT NOT NULL REFERENCES projects(id),
    goal_id         TEXT NOT NULL,
    plan_id         TEXT,
    state           TEXT NOT NULL,
    stop_condition  TEXT,
    iterations      INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

-- Goal revisions. A change creates a new row; in-flight results keep their
-- original goal_id (SPEC §4.1).
CREATE TABLE goals (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    revision    INTEGER NOT NULL,
    body        TEXT NOT NULL,          -- full Goal as JSON
    created_at  TEXT NOT NULL,
    UNIQUE (run_id, revision)
);

CREATE TABLE plans (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    goal_id     TEXT NOT NULL REFERENCES goals(id),
    revision    INTEGER NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (run_id, revision)
);

CREATE TABLE sources (
    id            TEXT PRIMARY KEY,
    run_id        TEXT NOT NULL REFERENCES runs(id),
    content_hash  TEXT NOT NULL,
    access        TEXT NOT NULL,
    body          TEXT NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_sources_run ON sources(run_id);

CREATE TABLE search_records (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

-- Immutable item content (SPEC §4.2). Never updated in place.
CREATE TABLE items (
    id            TEXT PRIMARY KEY,
    run_id        TEXT NOT NULL REFERENCES runs(id),
    kind          TEXT NOT NULL,
    goal_id       TEXT NOT NULL,
    content_hash  TEXT NOT NULL,
    body          TEXT NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_items_run_kind ON items(run_id, kind);

CREATE TABLE item_parents (
    child_id   TEXT NOT NULL REFERENCES items(id),
    parent_id  TEXT NOT NULL REFERENCES items(id),
    PRIMARY KEY (child_id, parent_id)
);

-- Mutable workflow state, separate from immutable content (SPEC §4.3).
CREATE TABLE item_states (
    item_id       TEXT PRIMARY KEY REFERENCES items(id),
    disposition   TEXT NOT NULL,
    duplicate_of  TEXT REFERENCES items(id),
    reason        TEXT,
    updated_at    TEXT NOT NULL
);

CREATE TABLE evidence (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    kind        TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_evidence_run ON evidence(run_id);

-- Evidence-to-claim links. One evidence item may support one claim and
-- contradict another, so the relation lives here (SPEC §4.3).
CREATE TABLE evidence_links (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    evidence_id  TEXT NOT NULL REFERENCES evidence(id),
    item_id      TEXT NOT NULL REFERENCES items(id),
    relation     TEXT NOT NULL,
    claim        TEXT NOT NULL,
    body         TEXT NOT NULL,
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_evlinks_item ON evidence_links(item_id);

CREATE TABLE reviews (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    item_id     TEXT NOT NULL REFERENCES items(id),
    item_hash   TEXT NOT NULL,
    strategy    TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_reviews_item ON reviews(item_id);

-- Assessment history. Append-only; the current assessment is the latest row.
CREATE TABLE assessments (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id         TEXT NOT NULL REFERENCES items(id),
    assessment      TEXT NOT NULL,
    basis_review    TEXT REFERENCES reviews(id),
    basis_decision  TEXT,
    body            TEXT NOT NULL,
    assessed_at     TEXT NOT NULL
);
CREATE INDEX idx_assessments_item ON assessments(item_id, id);

CREATE TABLE experiments (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    external    INTEGER NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE artifacts (
    id            TEXT PRIMARY KEY,
    run_id        TEXT NOT NULL REFERENCES runs(id),
    content_hash  TEXT NOT NULL,
    media_type    TEXT NOT NULL,
    access        TEXT NOT NULL,
    storage_path  TEXT NOT NULL,
    body          TEXT NOT NULL,
    created_at    TEXT NOT NULL
);

CREATE TABLE decisions (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    kind        TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_decisions_run ON decisions(run_id, created_at);

-- Tournament. A match is unique per (cohort, pair, task) so duplicate result
-- delivery after a crash cannot apply an Elo update twice (SPEC §9.2).
CREATE TABLE matches (
    id            TEXT PRIMARY KEY,
    run_id        TEXT NOT NULL REFERENCES runs(id),
    item_a        TEXT NOT NULL REFERENCES items(id),
    item_b        TEXT NOT NULL REFERENCES items(id),
    rubric_id     TEXT NOT NULL,
    outcome       TEXT NOT NULL,
    dedupe_key    TEXT NOT NULL UNIQUE,
    body          TEXT NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_matches_run ON matches(run_id);

-- Ratings are per (item, rubric revision): a new rubric starts a new cohort
-- at the initial rating and never mixes with the old one (SPEC §7).
CREATE TABLE ratings (
    item_id         TEXT NOT NULL REFERENCES items(id),
    rubric_id       TEXT NOT NULL,
    rating          REAL NOT NULL,
    matches_played  INTEGER NOT NULL DEFAULT 0,
    stale           INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT NOT NULL,
    PRIMARY KEY (item_id, rubric_id)
);

CREATE TABLE clusters (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE feedback (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    revision    INTEGER NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (run_id, revision)
);

-- Durable task queue (SPEC §9.2).
CREATE TABLE tasks (
    id                  TEXT PRIMARY KEY,
    run_id              TEXT NOT NULL REFERENCES runs(id),
    role                TEXT NOT NULL,
    strategy            TEXT NOT NULL,
    state               TEXT NOT NULL,
    priority            REAL NOT NULL,
    attempts            INTEGER NOT NULL DEFAULT 0,
    seed                INTEGER NOT NULL,
    input_hash          TEXT NOT NULL,
    -- Idempotency key: a completed task with the same key is reused rather
    -- than re-executed, so restart cannot duplicate records.
    dedupe_key          TEXT UNIQUE,
    body                TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);
CREATE INDEX idx_tasks_ready ON tasks(run_id, state, priority DESC);

CREATE TABLE task_deps (
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    depends_on  TEXT NOT NULL REFERENCES tasks(id),
    PRIMARY KEY (task_id, depends_on)
);

CREATE TABLE approvals (
    id             TEXT PRIMARY KEY,
    run_id         TEXT NOT NULL REFERENCES runs(id),
    task_id        TEXT REFERENCES tasks(id),
    state          TEXT NOT NULL,
    payload_hash   TEXT NOT NULL,
    body           TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    decided_at     TEXT
);
CREATE INDEX idx_approvals_run ON approvals(run_id, state);

-- Budget ledger. Reservations are released and spend is settled exactly once,
-- keyed by settlement id (SPEC §6, §9.2).
CREATE TABLE budget_ledger (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id          TEXT NOT NULL REFERENCES runs(id),
    task_id         TEXT REFERENCES tasks(id),
    settlement_key  TEXT NOT NULL UNIQUE,
    kind            TEXT NOT NULL,      -- reserve | settle | release
    model_calls     INTEGER NOT NULL DEFAULT 0,
    tokens          INTEGER NOT NULL DEFAULT 0,
    tool_executions INTEGER NOT NULL DEFAULT 0,
    cost_usd        REAL,               -- NULL means unknown, not zero
    created_at      TEXT NOT NULL
);

-- Campaign snapshots for scheduler feedback (SPEC §6).
CREATE TABLE snapshots (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    iteration   INTEGER NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

-- Persisted scheduler choices, so resume replays rather than redraws
-- nondeterministic decisions (SPEC §6, §9.2).
CREATE TABLE scheduler_choices (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    iteration   INTEGER NOT NULL,
    choice_key  TEXT NOT NULL,
    choice      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (run_id, iteration, choice_key)
);
