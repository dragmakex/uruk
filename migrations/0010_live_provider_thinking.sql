-- Live thinking previews include in-progress and interrupted provider streams,
-- not only completed request logs.
CREATE INDEX IF NOT EXISTS idx_request_logs_run_task_thinking
ON request_logs(run_id, task_id, started_at DESC, id DESC)
WHERE direction = 'provider';
