-- Operator-only diagnostics. Deliberately not exposed by any public API.
-- A committed 'in_progress' row exists before an operation is dispatched.
CREATE TABLE request_logs (
    id TEXT PRIMARY KEY,
    request_id TEXT NOT NULL,
    logical_id TEXT,
    direction TEXT NOT NULL CHECK(direction IN ('inbound','provider')),
    run_id TEXT,
    task_id TEXT,
    attempt INTEGER,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    duration_ms INTEGER,
    status INTEGER,
    outcome TEXT NOT NULL DEFAULT 'in_progress',
    request TEXT NOT NULL,
    response TEXT,
    error TEXT,
    retry_delay_ms INTEGER
);
CREATE INDEX idx_request_logs_request ON request_logs(request_id);
CREATE INDEX idx_request_logs_run ON request_logs(run_id,started_at);
CREATE INDEX idx_request_logs_logical ON request_logs(logical_id,attempt);
CREATE INDEX idx_request_logs_outcome ON request_logs(outcome,started_at);
