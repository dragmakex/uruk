-- Rollout phase 1: the local passage index (FTS5). The extracted-text
-- artifact file remains the canonical full text; `passages` is a derived
-- index whose `artifact_hash` makes drift between file and index detectable.

-- Passages: deterministic chunks of extracted text artifacts. Append-only.
CREATE TABLE passages (
    id            INTEGER PRIMARY KEY,   -- rowid, FTS5 content_rowid
    run_id        TEXT NOT NULL REFERENCES runs(id),
    source_id     TEXT NOT NULL REFERENCES sources(id),
    artifact_id   TEXT NOT NULL,
    artifact_hash TEXT NOT NULL,         -- ContentHash of the full artifact text;
                                         -- detects drift between file and index
    seq           INTEGER NOT NULL,      -- 0-based chunk ordinal within the source
    byte_start    INTEGER NOT NULL,      -- byte offsets into the UTF-8 artifact text
    byte_end      INTEGER NOT NULL,
    text          TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    UNIQUE (source_id, seq)
);
CREATE INDEX idx_passages_run ON passages(run_id);

-- FTS5 external-content index over passages.text.
CREATE VIRTUAL TABLE passages_fts USING fts5(
    text,
    content='passages',
    content_rowid='id',
    tokenize='porter unicode61 remove_diacritics 2'
);

-- Explicit sync choice: external content + full trigger set. Passages are
-- append-only in practice, but delete/update triggers keep the index
-- impossible to desynchronize if a future migration prunes rows.
CREATE TRIGGER passages_ai AFTER INSERT ON passages BEGIN
    INSERT INTO passages_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER passages_ad AFTER DELETE ON passages BEGIN
    INSERT INTO passages_fts(passages_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER passages_au AFTER UPDATE ON passages BEGIN
    INSERT INTO passages_fts(passages_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO passages_fts(rowid, text) VALUES (new.id, new.text);
END;
