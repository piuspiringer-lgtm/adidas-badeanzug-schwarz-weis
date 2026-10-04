//! Dauerhaftes Gedächtnis von JARVIS auf Basis von SQLite (+ FTS5).
//!
//! Enthält: Präferenzen, Fakten, Workflows und deren Läufe, Skills,
//! Tool-Erfahrungen, Fehler/Lösungen, den Research-Cache und das
//! hash-verkettete Audit-Log.

mod audit;
mod research;
mod schema;

pub use audit::{AuditEntry, SqliteAudit};
pub use research::{CachedPage, KnowledgeHit};

use jarvis_permissions::is_hard_denied;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("ungültig: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, MemoryError>;

pub(crate) fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowStep {
    pub tool: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: i64,
    pub name: String,
    pub intent: String,
    pub steps: Vec<WorkflowStep>,
    pub successes: i64,
    pub failures: i64,
}

impl Workflow {
    /// Laplace-geglättete Erfolgsquote.
    pub fn score(&self) -> f64 {
        (self.successes as f64 + 1.0) / (self.successes as f64 + self.failures as f64 + 2.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub steps: Vec<WorkflowStep>,
    pub version: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolStat {
    pub tool: String,
    pub intent: String,
    pub successes: i64,
    pub failures: i64,
    pub avg_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorSolution {
    pub signature: String,
    pub error: String,
    pub solution: String,
    pub hits: i64,
}

/// Thread-sicherer Zugriff auf die Datenbank.
#[derive(Clone)]
pub struct Memory {
    pub(crate) conn: Arc<Mutex<Connection>>,
}

impl Memory {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.lock().unwrap().query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    // ---------- Präferenzen ----------
    pub fn set_preference(&self, key: &str, value: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO preferences(key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now()],
        )?;
        Ok(())
    }

    pub fn preference(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT value FROM preferences WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    // ---------- Fakten ----------
    pub fn remember_fact(&self, topic: &str, fact: &str, source: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT OR IGNORE INTO facts(topic, fact, source, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![topic, fact, source, now()],
        )?;
        Ok(())
    }

    pub fn search_facts(&self, query: &str, limit: usize) -> Result<Vec<(String, String)>> {
        let q = research::fts_query(query);
        if q.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT f.topic, f.fact FROM facts_fts JOIN facts f ON f.id = facts_fts.rowid
             WHERE facts_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = st.query_map(params![q, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---------- Workflows ----------
    fn check_steps(steps: &[WorkflowStep]) -> Result<()> {
        if let Some(s) = steps.iter().find(|s| is_hard_denied(&s.tool)) {
            return Err(MemoryError::Invalid(format!("Schritt nutzt gesperrte Aktion '{}'", s.tool)));
        }
        Ok(())
    }

    pub fn save_workflow(&self, name: &str, intent: &str, steps: &[WorkflowStep]) -> Result<i64> {
        Self::check_steps(steps)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workflows(name, intent, steps_json, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET intent = excluded.intent, steps_json = excluded.steps_json",
            params![name, intent, serde_json::to_string(steps)?, now()],
        )?;
        Ok(conn.query_row("SELECT id FROM workflows WHERE name = ?1", [name], |r| r.get(0))?)
    }

    pub fn record_workflow_run(&self, workflow_id: i64, success: bool, duration_ms: i64, note: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workflow_runs(workflow_id, success, duration_ms, note, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![workflow_id, success, duration_ms, note, now()],
        )?;
        let col = if success { "successes" } else { "failures" };
        conn.execute(
            &format!("UPDATE workflows SET {col} = {col} + 1, last_used = ?2 WHERE id = ?1"),
            params![workflow_id, now()],
        )?;
        Ok(())
    }

    /// Workflows passend zum Intent, sortiert nach Erfolgsquote.
    pub fn find_workflows(&self, intent: &str, limit: usize) -> Result<Vec<Workflow>> {
        let q = research::fts_query(intent);
        if q.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT w.id, w.name, w.intent, w.steps_json, w.successes, w.failures
             FROM workflows_fts JOIN workflows w ON w.id = workflows_fts.rowid
             WHERE workflows_fts MATCH ?1 LIMIT 50",
        )?;
        let mut v: Vec<Workflow> = st
            .query_map([q], |r| {
                let steps: String = r.get(3)?;
                Ok(Workflow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    intent: r.get(2)?,
                    steps: serde_json::from_str(&steps).unwrap_or_default(),
                    successes: r.get(4)?,
                    failures: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        v.sort_by(|a, b| b.score().partial_cmp(&a.score()).unwrap());
        v.truncate(limit);
        Ok(v)
    }

    // ---------- Skills ----------
    /// Speichert einen Skill. Neue Skills sind deaktiviert, bis der Benutzer
    /// sie in der UI freigibt (`set_skill_enabled` wird nur vom Host aufgerufen).
    pub fn save_skill(&self, name: &str, description: &str, steps: &[WorkflowStep]) -> Result<i64> {
        Self::check_steps(steps)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO skills(name, description, steps_json, version, enabled, created_at)
             VALUES (?1, ?2, ?3, 1, 0, ?4)
             ON CONFLICT(name) DO UPDATE SET description = excluded.description,
               steps_json = excluded.steps_json, version = skills.version + 1, enabled = 0",
            params![name, description, serde_json::to_string(steps)?, now()],
        )?;
        Ok(conn.query_row("SELECT version FROM skills WHERE name = ?1", [name], |r| r.get(0))?)
    }

    pub fn set_skill_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        self.conn.lock().unwrap().execute("UPDATE skills SET enabled = ?2 WHERE name = ?1", params![name, enabled])?;
        Ok(())
    }

    pub fn skill(&self, name: &str) -> Result<Option<Skill>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT name, description, steps_json, version, enabled FROM skills WHERE name = ?1",
                [name],
                |r| {
                    let steps: String = r.get(2)?;
                    Ok(Skill {
                        name: r.get(0)?,
                        description: r.get(1)?,
                        steps: serde_json::from_str(&steps).unwrap_or_default(),
                        version: r.get(3)?,
                        enabled: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    // ---------- Tool-Erfahrungen ----------
    pub fn record_tool_result(&self, tool: &str, intent: &str, success: bool, duration_ms: f64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO tool_stats(tool, intent, successes, failures, avg_ms, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(tool, intent) DO UPDATE SET
               successes = successes + excluded.successes,
               failures = failures + excluded.failures,
               avg_ms = (avg_ms * (successes + failures) + excluded.avg_ms) / (successes + failures + 1),
               updated_at = excluded.updated_at",
            params![tool, intent, success as i64, (!success) as i64, duration_ms, now()],
        )?;
        Ok(())
    }

    pub fn tool_stats(&self, intent: &str) -> Result<Vec<ToolStat>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT tool, intent, successes, failures, avg_ms FROM tool_stats WHERE intent = ?1
             ORDER BY (successes + 1.0) / (successes + failures + 2.0) DESC",
        )?;
        let rows = st.query_map([intent], |r| {
            Ok(ToolStat { tool: r.get(0)?, intent: r.get(1)?, successes: r.get(2)?, failures: r.get(3)?, avg_ms: r.get(4)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---------- Fehler & Lösungen ----------
    pub fn record_error_solution(&self, signature: &str, error: &str, solution: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO errors_solutions(signature, error, solution, hits, updated_at) VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(signature) DO UPDATE SET solution = excluded.solution, hits = hits + 1, updated_at = excluded.updated_at",
            params![signature, error, solution, now()],
        )?;
        Ok(())
    }

    pub fn solution_for(&self, signature: &str) -> Result<Option<ErrorSolution>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT signature, error, solution, hits FROM errors_solutions WHERE signature = ?1",
                [signature],
                |r| Ok(ErrorSolution { signature: r.get(0)?, error: r.get(1)?, solution: r.get(2)?, hits: r.get(3)? }),
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(tool: &str) -> WorkflowStep {
        WorkflowStep { tool: tool.into(), args: json!({}) }
    }

    #[test]
    fn migrations_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("brain.db");
        {
            let m = Memory::open(&p).unwrap();
            m.set_preference("sprache", "de").unwrap();
        }
        let m = Memory::open(&p).unwrap();
        assert_eq!(m.preference("sprache").unwrap().as_deref(), Some("de"));
        assert_eq!(m.schema_version().unwrap(), schema::VERSION);
    }

    #[test]
    fn workflows_ranked_by_success() {
        let m = Memory::in_memory().unwrap();
        let a = m.save_workflow("stundenplan_a", "stundenplan morgen anzeigen", &[step("webuntis_timetable")]).unwrap();
        let b = m.save_workflow("stundenplan_b", "stundenplan woche anzeigen", &[step("webuntis_timetable")]).unwrap();
        m.record_workflow_run(a, false, 10, "").unwrap();
        m.record_workflow_run(b, true, 10, "").unwrap();
        m.record_workflow_run(b, true, 10, "").unwrap();
        let found = m.find_workflows("stundenplan", 5).unwrap();
        assert_eq!(found[0].name, "stundenplan_b");
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn workflows_and_skills_cannot_contain_denied_actions() {
        let m = Memory::in_memory().unwrap();
        for a in ["send_email", "delete_email", "send_teams_message", "edit_teams_message", "modify_webuntis"] {
            assert!(m.save_workflow("x", "y", &[step(a)]).is_err(), "{a}");
            assert!(m.save_skill("x", "y", &[step("web_search"), step(a)]).is_err(), "{a}");
        }
    }

    #[test]
    fn skills_start_disabled_and_reset_on_change() {
        let m = Memory::in_memory().unwrap();
        assert_eq!(m.save_skill("morgen", "Tagesüberblick", &[step("webuntis_timetable")]).unwrap(), 1);
        assert!(!m.skill("morgen").unwrap().unwrap().enabled);
        m.set_skill_enabled("morgen", true).unwrap();
        assert!(m.skill("morgen").unwrap().unwrap().enabled);
        assert_eq!(m.save_skill("morgen", "Tagesüberblick v2", &[step("email_search")]).unwrap(), 2);
        assert!(!m.skill("morgen").unwrap().unwrap().enabled, "Änderung erfordert neue Freigabe");
    }

    #[test]
    fn tool_stats_and_errors() {
        let m = Memory::in_memory().unwrap();
        m.record_tool_result("web_search", "recherche", true, 100.0).unwrap();
        m.record_tool_result("web_search", "recherche", false, 300.0).unwrap();
        m.record_tool_result("web_fetch", "recherche", true, 50.0).unwrap();
        let s = m.tool_stats("recherche").unwrap();
        assert_eq!(s[0].tool, "web_fetch");
        assert_eq!(s[1].successes, 1);
        assert_eq!(s[1].failures, 1);
        assert!((s[1].avg_ms - 200.0).abs() < 0.01);

        m.record_error_solution("ollama:connection_refused", "Verbindung abgelehnt", "ollama serve starten").unwrap();
        m.record_error_solution("ollama:connection_refused", "Verbindung abgelehnt", "Dienst neu laden").unwrap();
        let e = m.solution_for("ollama:connection_refused").unwrap().unwrap();
        assert_eq!(e.hits, 2);
        assert_eq!(e.solution, "Dienst neu laden");
    }

    #[test]
    fn facts_fulltext() {
        let m = Memory::in_memory().unwrap();
        m.remember_fact("schule", "Mathe-Schularbeit am 12. November", "webuntis").unwrap();
        m.remember_fact("privat", "Lieblingsfarbe ist blau", "user").unwrap();
        let hits = m.search_facts("Schularbeit Mathe", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].1.contains("November"));
    }
}
