-- V010: Reserve shared_doc table for post-v1 Loro integration.
--
-- WHY: per architecture audit §4.8 pre-v1 action 2. Empty table;
-- exact shape prescribed by the audit. Locking the shape NOW lets
-- the post-v1 integration skip a schema break. Loro will persist
-- one doc per library (tag taxonomy, collections, saved searches,
-- reference-board layouts, edit decision lists) — the subset of
-- shared state where CRDT semantics earn their cost.

CREATE TABLE shared_doc (
    id             TEXT PRIMARY KEY,
    snapshot       BLOB,
    version_vector BLOB,
    updated_at     INTEGER NOT NULL,
    hlc            INTEGER
);
