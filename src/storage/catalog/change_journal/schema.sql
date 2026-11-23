CREATE TABLE change_journal_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    sequence INTEGER NOT NULL CHECK(typeof(sequence) = 'integer' AND sequence >= 0),
    floor INTEGER NOT NULL CHECK(floor >= 0 AND floor <= sequence),
    epoch INTEGER NOT NULL CHECK(typeof(epoch) = 'integer' AND epoch >= 0),
    parent_pap TEXT NOT NULL
) WITHOUT ROWID;
INSERT INTO change_journal_state VALUES (1, 0, 0, 0, 0, '[]');
CREATE TABLE change_journal (
    sequence INTEGER PRIMARY KEY CHECK(sequence > 0),
    origin TEXT NOT NULL CHECK(origin IN ('api', 'seed', 'bucket-init', 'sync-ingest', 'import')),
    kind TEXT NOT NULL CHECK(kind IN ('publish', 'metadata', 'delete', 'bucket-create', 'bucket-update', 'bucket-delete', 'iam')),
    bucket TEXT NOT NULL,
    project TEXT NOT NULL,
    incarnation TEXT NOT NULL,
    object TEXT,
    generation INTEGER,
    metageneration INTEGER
);
PRAGMA user_version = 15;
