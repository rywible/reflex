CREATE TABLE IF NOT EXISTS experiments (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    domain TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS cells (
    id TEXT PRIMARY KEY,
    experiment_id TEXT NOT NULL REFERENCES experiments(id),
    generation_id TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    resource_class TEXT NOT NULL,
    priority INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'ready',
    worker_id TEXT,
    attempt_no INTEGER NOT NULL DEFAULT 0,
    accepted_attempt_no INTEGER,
    fencing_token INTEGER NOT NULL DEFAULT 0,
    lease_expires_at INTEGER,
    completion_manifest_digest TEXT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS artifacts (
    cell_id TEXT NOT NULL REFERENCES cells(id),
    digest TEXT NOT NULL,
    kind TEXT NOT NULL,
    PRIMARY KEY (cell_id, digest)
);

CREATE TABLE IF NOT EXISTS active_models (
    role TEXT PRIMARY KEY,
    checkpoint_id TEXT NOT NULL,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
