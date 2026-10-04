use crate::{now, Memory, Result};
use jarvis_runtime::{AuditEvent, AuditSink};
use rusqlite::params;
use serde::Serialize;
use sha2::{Digest, Sha256};

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Schlüssel, deren Werte nie im Klartext ins Audit-Log gelangen.
const SECRET_KEYS: &[&str] = &["password", "passwort", "token", "secret", "api_key", "authorization"];

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: i64,
    pub tool: String,
    pub origin: String,
    pub args_json: String,
    pub decision: String,
    pub outcome: String,
}

/// Audit-Log in SQLite. Jeder Eintrag enthält den Hash des Vorgängers,
/// nachträgliche Manipulation wird von `verify` erkannt; UPDATE/DELETE
/// verhindern Trigger.
#[derive(Clone)]
pub struct SqliteAudit {
    mem: Memory,
}

fn redact(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if SECRET_KEYS.iter().any(|s| k.to_ascii_lowercase().contains(s)) {
                    *val = serde_json::Value::String("***".into());
                } else {
                    redact(val);
                }
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(redact),
        _ => {}
    }
}

fn hash(prev: &str, ts: i64, tool: &str, origin: &str, args: &str, decision: &str, outcome: &str) -> String {
    let mut h = Sha256::new();
    for part in [prev, &ts.to_string(), tool, origin, args, decision, outcome] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    hex::encode(h.finalize())
}

impl SqliteAudit {
    pub fn new(mem: Memory) -> Self {
        Self { mem }
    }

    pub fn append(&self, e: &AuditEvent) -> Result<()> {
        let mut args = e.args.clone();
        redact(&mut args);
        let args = serde_json::to_string(&args)?;
        let origin = format!("{:?}", e.origin);
        let conn = self.mem.conn.lock().unwrap();
        let prev: String = conn
            .query_row("SELECT hash FROM audit_log ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .unwrap_or_else(|_| GENESIS.to_string());
        let ts = now();
        let h = hash(&prev, ts, &e.tool, &origin, &args, &e.decision, &e.outcome);
        conn.execute(
            "INSERT INTO audit_log(ts, tool, origin, args_json, decision, outcome, duration_ms, prev_hash, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![ts, e.tool, origin, args, e.decision, e.outcome, e.duration_ms as i64, prev, h],
        )?;
        Ok(())
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>> {
        let conn = self.mem.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT id, ts, tool, origin, args_json, decision, outcome FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(AuditEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                tool: r.get(2)?,
                origin: r.get(3)?,
                args_json: r.get(4)?,
                decision: r.get(5)?,
                outcome: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Prüft die gesamte Hash-Kette. Gibt die ID des ersten defekten Eintrags zurück.
    pub fn verify(&self) -> Result<std::result::Result<usize, i64>> {
        let conn = self.mem.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT id, ts, tool, origin, args_json, decision, outcome, prev_hash, hash FROM audit_log ORDER BY id",
        )?;
        let mut rows = st.query([])?;
        let mut prev = GENESIS.to_string();
        let mut n = 0;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let p: String = r.get(7)?;
            let stored: String = r.get(8)?;
            let calc = hash(&prev, r.get(1)?, &r.get::<_, String>(2)?, &r.get::<_, String>(3)?, &r.get::<_, String>(4)?, &r.get::<_, String>(5)?, &r.get::<_, String>(6)?);
            if p != prev || calc != stored {
                return Ok(Err(id));
            }
            prev = stored;
            n += 1;
        }
        Ok(Ok(n))
    }
}

impl AuditSink for SqliteAudit {
    fn record(&self, event: &AuditEvent) {
        if let Err(e) = self.append(event) {
            // Audit darf nie still scheitern.
            eprintln!("[jarvis-audit] Schreiben fehlgeschlagen: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_permissions::Origin;
    use serde_json::json;

    fn ev(tool: &str) -> AuditEvent {
        AuditEvent {
            tool: tool.into(),
            origin: Origin::Agent,
            args: json!({"query": "x", "password": "geheim", "nested": {"api_key": "k"}}),
            decision: "allowed".into(),
            outcome: "ok".into(),
            duration_ms: 3,
        }
    }

    #[test]
    fn chain_verifies_and_secrets_are_redacted() {
        let a = SqliteAudit::new(Memory::in_memory().unwrap());
        a.record(&ev("web_search"));
        a.record(&ev("send_email"));
        assert_eq!(a.verify().unwrap(), Ok(2));
        let r = a.recent(10).unwrap();
        assert_eq!(r[0].tool, "send_email");
        assert!(!r[0].args_json.contains("geheim"));
        assert!(!r[0].args_json.contains("\"k\""));
    }

    #[test]
    fn audit_log_is_append_only() {
        let mem = Memory::in_memory().unwrap();
        let a = SqliteAudit::new(mem.clone());
        a.record(&ev("web_search"));
        let conn = mem.conn.lock().unwrap();
        assert!(conn.execute("UPDATE audit_log SET outcome = 'x'", []).is_err());
        assert!(conn.execute("DELETE FROM audit_log", []).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let mem = Memory::in_memory().unwrap();
        let a = SqliteAudit::new(mem.clone());
        a.record(&ev("a"));
        a.record(&ev("b"));
        a.record(&ev("c"));
        {
            let conn = mem.conn.lock().unwrap();
            conn.execute_batch("DROP TRIGGER audit_no_update; UPDATE audit_log SET tool = 'manipuliert' WHERE id = 2;").unwrap();
        }
        assert_eq!(a.verify().unwrap(), Err(2));
    }
}
