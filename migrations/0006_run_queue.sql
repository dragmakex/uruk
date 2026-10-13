-- The durable FIFO run queue and the research clock (docs/WEB.md "One run
-- at a time").
--
-- Web runs execute one at a time, in the order they were enqueued. The
-- order is a dedicated sequence (`seq`, never reused), not a timestamp:
-- clocks can disagree, the sequence cannot. A web run's row is
-- `preparing` from creation (it holds its place but its start request
-- has not yet recorded the plan that makes it executable), `queued` once
-- the plan commits, `active` while the dispatcher drives the run, and
-- `done` once the run leaves the queue for any reason (finished,
-- cancelled, blocked, parked for a human, or abandoned because its start
-- request died while `preparing`). Exactly one row per run is live
-- (`preparing`, `queued`, or `active`) at a time; `done` rows stay as
-- history, and a run that must take another turn (parked, then released)
-- gets a new row at the tail.
--
-- The research clock on `runs`: `started_at` is stamped once, when the
-- run first executes, and the wall-clock budget runs from it rather than
-- from creation, so time spent queued is not research time. A transient
-- model-provider outage pauses the clock: `paused_since` is the open
-- wait, `paused_secs` the settled waits, `pause_note` the last reported
-- transient failure. An open wait survives a restart and is settled when
-- the provider serves again.

CREATE TABLE run_queue (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id        TEXT NOT NULL REFERENCES runs(id),
    state         TEXT NOT NULL,          -- preparing | queued | active | done
    enqueued_at   TEXT NOT NULL,
    activated_at  TEXT,
    finished_at   TEXT
);
CREATE INDEX idx_run_queue_live ON run_queue(state, seq);
CREATE INDEX idx_run_queue_run ON run_queue(run_id, state);

ALTER TABLE runs ADD COLUMN started_at TEXT;
ALTER TABLE runs ADD COLUMN paused_secs INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN paused_since TEXT;
ALTER TABLE runs ADD COLUMN pause_note TEXT;

-- Runs that predate the queue had already started when they were created
-- and dispatched; their clock keeps running from creation, as before.
UPDATE runs SET started_at = created_at WHERE started_at IS NULL;

-- A run left `running` by a pre-queue server is work this server must
-- still drive; it takes its place in creation order. Parked and finished
-- runs are not enqueued: the serve-side sweep re-enqueues a parked run
-- once its approvals are decided.
INSERT INTO run_queue (run_id, state, enqueued_at)
SELECT id, 'queued', created_at FROM runs
WHERE state = 'running'
ORDER BY created_at, id;
