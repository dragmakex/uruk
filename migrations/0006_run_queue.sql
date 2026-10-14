-- The durable run queue and the research clock (docs/WEB.md "One run at
-- a time", "Taking turns").
--
-- Web runs execute one at a time. Within one owner they run in the order
-- they were enqueued; between owners the least recently served owner
-- goes next. `seq` is a dedicated sequence (never reused), not a
-- timestamp: clocks can disagree, the sequence cannot. `owner` is the
-- run's browser identity (its one-way digest, never exposed over the
-- API), or `engine` for runs without one (engine callers, the sweep,
-- runs predating ownership), which take turns as one owner.
--
-- Selection: the active entry, if any; otherwise each owner's candidate
-- is its oldest live entry, eligible only while `queued` (an owner whose
-- oldest entry is still `preparing` is not eligible, so its later runs
-- cannot pass it), and the eligible owner with the smallest
-- `queue_owners.last_served` wins, ties broken by the candidate's `seq`.
-- Activation increments the global `queue_turns.turn`, stamps the owner
-- with it, and claims the entry in one transaction, so a restart
-- continues the turn order exactly and an interrupted active run always
-- resumes first. An owner never served has `last_served` 0 and is the
-- most deserving.
--
-- A web run's row is `preparing` from creation (it holds its place but
-- its start request has not yet recorded the plan that makes it
-- executable), `queued` once the plan commits, `active` while the
-- dispatcher drives the run, and `done` once the run leaves the queue for
-- any reason (finished, cancelled, blocked, parked for a human, or
-- abandoned because its start request died while `preparing`). Exactly
-- one row per run is live (`preparing`, `queued`, or `active`) at a time;
-- `done` rows stay as history, and a run that must take another turn
-- (parked, then released) gets a new row at the tail.
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
    owner         TEXT NOT NULL,          -- owner digest, or 'engine'
    state         TEXT NOT NULL,          -- preparing | queued | active | done
    enqueued_at   TEXT NOT NULL,
    activated_at  TEXT,
    finished_at   TEXT
);
CREATE INDEX idx_run_queue_live ON run_queue(state, seq);
CREATE INDEX idx_run_queue_run ON run_queue(run_id, state);
CREATE INDEX idx_run_queue_owner ON run_queue(owner, state);

-- The global dispatch turn: one row, incremented per activation.
CREATE TABLE queue_turns (
    id     INTEGER PRIMARY KEY CHECK (id = 1),
    turn   INTEGER NOT NULL
);
INSERT INTO queue_turns (id, turn) VALUES (1, 0);

-- The turn at which each owner was last served; absent means never.
CREATE TABLE queue_owners (
    owner        TEXT PRIMARY KEY,
    last_served  INTEGER NOT NULL
);

ALTER TABLE runs ADD COLUMN started_at TEXT;
ALTER TABLE runs ADD COLUMN paused_secs INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN paused_since TEXT;
ALTER TABLE runs ADD COLUMN pause_note TEXT;

-- Runs that predate the queue had already started when they were created
-- and dispatched; their clock keeps running from creation, as before.
UPDATE runs SET started_at = created_at WHERE started_at IS NULL;

-- A run left `running` by a pre-queue server is work this server must
-- still drive; it takes its place in creation order under its owner (or
-- the engine owner). No owner has been served yet. Parked and finished
-- runs are not enqueued: the serve-side sweep re-enqueues a parked run
-- once its approvals are decided.
INSERT INTO run_queue (run_id, owner, state, enqueued_at)
SELECT r.id, COALESCE(o.owner_digest, 'engine'), 'queued', r.created_at
FROM runs r LEFT JOIN run_owners o ON o.run_id = r.id
WHERE r.state = 'running'
ORDER BY r.created_at, r.id;
