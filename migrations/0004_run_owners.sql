-- Anonymous browser ownership for web-created runs (docs/WEB.md).
--
-- A row binds a run to one anonymous browser identity: the lowercase-hex
-- SHA-256 digest of the browser's opaque cookie token. The raw token is
-- never stored anywhere. Runs without a row — CLI runs, and every run
-- created before this table existed — are not reachable over the web API
-- at all: web access fails closed instead of exposing them or letting the
-- first visitor claim them.

CREATE TABLE run_owners (
    run_id        TEXT PRIMARY KEY REFERENCES runs(id),
    owner_digest  TEXT NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_run_owners_digest ON run_owners(owner_digest);
