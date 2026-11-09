-- Keep live agent reasoning previews cheap as request history grows.
CREATE INDEX IF NOT EXISTS idx_request_logs_run_task_latest
ON request_logs(run_id, task_id, started_at DESC, id DESC)
WHERE direction = 'provider' AND outcome = 'success';
