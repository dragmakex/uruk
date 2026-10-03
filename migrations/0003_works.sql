-- Rollout phase 2: works discovered by federated search, deduplicated per
-- run. The merged WorkRecord JSON in `body` carries the pre-dedup
-- per-connector hits (connector, search_id, rank, raw_hash) under
-- `body.hits`, so the RRF computation stays re-derivable without a
-- relational hit table. `search_records` keeps its JSON-body convention.
CREATE TABLE works (
    work_key    TEXT NOT NULL,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    doi         TEXT,
    arxiv_id    TEXT,
    pmid        TEXT,
    title       TEXT NOT NULL,
    year        INTEGER,
    rrf_score   REAL,                -- fused score after dedup (nullable until fusion)
    source_id   TEXT REFERENCES sources(id),  -- set when acquired/ingested
    body        TEXT NOT NULL,       -- merged WorkRecord JSON, including hits
    created_at  TEXT NOT NULL,
    PRIMARY KEY (run_id, work_key)
);
CREATE INDEX idx_works_run ON works(run_id, rrf_score DESC);
