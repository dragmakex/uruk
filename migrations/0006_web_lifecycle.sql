-- Durable browser pause accounting and canonical restart intent.
ALTER TABLE runs ADD COLUMN paused_at TEXT;
ALTER TABLE runs ADD COLUMN paused_ms INTEGER NOT NULL DEFAULT 0;

CREATE TABLE web_run_configs (
    run_id      TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
