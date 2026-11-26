-- Ordered page reads must not sort/materialize every history body first.
CREATE INDEX idx_matches_run_page ON matches(run_id, created_at, id);
CREATE INDEX idx_decisions_run_page ON decisions(run_id, created_at, id);

-- Live snapshots read counts and one prior checkpoint, never full inventories.
CREATE INDEX idx_reviews_run ON reviews(run_id);
CREATE INDEX idx_clusters_run ON clusters(run_id);
CREATE INDEX idx_search_records_run ON search_records(run_id);
CREATE INDEX idx_snapshots_run_iteration ON snapshots(run_id, iteration, id);
