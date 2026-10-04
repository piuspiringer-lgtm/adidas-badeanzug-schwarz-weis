use rusqlite::Connection;

pub const VERSION: i64 = 1;

const V1: &str = r#"
CREATE TABLE preferences (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE facts (
    id INTEGER PRIMARY KEY,
    topic TEXT NOT NULL,
    fact TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(topic, fact)
);
CREATE VIRTUAL TABLE facts_fts USING fts5(topic, fact, content='facts', content_rowid='id');
CREATE TRIGGER facts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, topic, fact) VALUES (new.id, new.topic, new.fact);
END;
CREATE TRIGGER facts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, topic, fact) VALUES ('delete', old.id, old.topic, old.fact);
END;

CREATE TABLE workflows (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    intent TEXT NOT NULL,
    steps_json TEXT NOT NULL,
    successes INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    last_used INTEGER,
    created_at INTEGER NOT NULL
);
CREATE VIRTUAL TABLE workflows_fts USING fts5(name, intent, content='workflows', content_rowid='id');
CREATE TRIGGER workflows_ai AFTER INSERT ON workflows BEGIN
    INSERT INTO workflows_fts(rowid, name, intent) VALUES (new.id, new.name, new.intent);
END;
CREATE TRIGGER workflows_au AFTER UPDATE OF name, intent ON workflows BEGIN
    INSERT INTO workflows_fts(workflows_fts, rowid, name, intent) VALUES ('delete', old.id, old.name, old.intent);
    INSERT INTO workflows_fts(rowid, name, intent) VALUES (new.id, new.name, new.intent);
END;

CREATE TABLE workflow_runs (
    id INTEGER PRIMARY KEY,
    workflow_id INTEGER NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    success INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    note TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE skills (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL,
    steps_json TEXT NOT NULL,
    version INTEGER NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE TABLE tool_stats (
    tool TEXT NOT NULL,
    intent TEXT NOT NULL,
    successes INTEGER NOT NULL,
    failures INTEGER NOT NULL,
    avg_ms REAL NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (tool, intent)
);

CREATE TABLE errors_solutions (
    signature TEXT PRIMARY KEY,
    error TEXT NOT NULL,
    solution TEXT NOT NULL,
    hits INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Research-Cache
CREATE TABLE research_queries (
    query_norm TEXT NOT NULL,
    provider TEXT NOT NULL,
    results_json TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    PRIMARY KEY (query_norm, provider)
);
CREATE TABLE research_pages (
    url TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    text TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    fetched_at INTEGER NOT NULL
);
CREATE TABLE research_chunks (
    id INTEGER PRIMARY KEY,
    url TEXT NOT NULL REFERENCES research_pages(url) ON DELETE CASCADE,
    ord INTEGER NOT NULL,
    text TEXT NOT NULL,
    chunk_hash TEXT NOT NULL UNIQUE
);
CREATE VIRTUAL TABLE research_chunks_fts USING fts5(text, content='research_chunks', content_rowid='id');
CREATE TRIGGER research_chunks_ai AFTER INSERT ON research_chunks BEGIN
    INSERT INTO research_chunks_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER research_chunks_ad AFTER DELETE ON research_chunks BEGIN
    INSERT INTO research_chunks_fts(research_chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;

-- Audit-Log: nur anhängen, hash-verkettet
CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    tool TEXT NOT NULL,
    origin TEXT NOT NULL,
    args_json TEXT NOT NULL,
    decision TEXT NOT NULL,
    outcome TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    prev_hash TEXT NOT NULL,
    hash TEXT NOT NULL
);
CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit_log BEGIN
    SELECT RAISE(ABORT, 'audit_log ist unveränderlich');
END;
CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit_log BEGIN
    SELECT RAISE(ABORT, 'audit_log ist unveränderlich');
END;
"#;

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current < 1 {
        conn.execute_batch(&format!("BEGIN; {V1} PRAGMA user_version = {VERSION}; COMMIT;"))?;
    }
    Ok(())
}
