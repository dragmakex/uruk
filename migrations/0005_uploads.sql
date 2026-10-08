-- Owner-bound file uploads for the web API (docs/WEB.md).
--
-- A browser never supplies a server path: it posts bytes, the server
-- stores them under `.uruk/uploads/<owner_digest>/<upload_id>` and hands
-- back an opaque id. A row binds that id to one anonymous browser
-- identity (the SHA-256 digest of its cookie token, as in `run_owners`).
-- Every read and delete is owner-scoped, so one browser's uploads are
-- unreachable from any other; `storage_path` is relative to the project
-- directory and never crosses the wire.

CREATE TABLE uploads (
    id            TEXT PRIMARY KEY,
    owner_digest  TEXT NOT NULL,
    file_name     TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    content_hash  TEXT NOT NULL,
    storage_path  TEXT NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_uploads_owner ON uploads(owner_digest);
