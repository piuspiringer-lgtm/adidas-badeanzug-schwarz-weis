//! Research-Cache: Suchergebnisse, extrahierte Seiten und deren Abschnitte.
//! Bereits recherchiertes Wissen wird per Volltextsuche wiederverwendet,
//! statt Seiten erneut zu laden.

use crate::{now, Memory, Result};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize)]
pub struct CachedPage {
    pub url: String,
    pub title: String,
    pub text: String,
    pub content_hash: String,
    pub fetched_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeHit {
    pub url: String,
    pub title: String,
    pub text: String,
    /// BM25-Rang von FTS5 (kleiner = besser).
    pub rank: f64,
}

pub fn content_hash(text: &str) -> String {
    let norm: String = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    hex::encode(Sha256::digest(norm.as_bytes()))
}

pub fn normalize_query(q: &str) -> String {
    let mut words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    words.sort();
    words.dedup();
    words.join(" ")
}

/// Baut eine sichere FTS5-Abfrage (jedes Wort in Anführungszeichen, ODER-verknüpft).
pub(crate) fn fts_query(q: &str) -> String {
    q.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| format!("\"{}\"", w.to_lowercase()))
        .collect::<Vec<_>>()
        .join(" OR ")
}

impl Memory {
    pub fn cache_search(&self, query: &str, provider: &str, results_json: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO research_queries(query_norm, provider, results_json, fetched_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(query_norm, provider) DO UPDATE SET results_json = excluded.results_json, fetched_at = excluded.fetched_at",
            params![normalize_query(query), provider, results_json, now()],
        )?;
        Ok(())
    }

    pub fn cached_search(&self, query: &str, provider: &str, max_age_secs: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT results_json FROM research_queries WHERE query_norm = ?1 AND provider = ?2 AND fetched_at >= ?3",
                params![normalize_query(query), provider, now() - max_age_secs],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Speichert eine extrahierte Seite samt Abschnitten. Abschnitte, die
    /// (inhaltsgleich) schon von einer anderen Seite bekannt sind, werden
    /// nicht doppelt gespeichert. Gibt die Anzahl neuer Abschnitte zurück.
    pub fn cache_page(&self, url: &str, title: &str, text: &str, chunks: &[String]) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM research_chunks WHERE url = ?1", [url])?;
        tx.execute(
            "INSERT INTO research_pages(url, title, text, content_hash, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(url) DO UPDATE SET title = excluded.title, text = excluded.text,
               content_hash = excluded.content_hash, fetched_at = excluded.fetched_at",
            params![url, title, text, content_hash(text), now()],
        )?;
        let mut added = 0;
        for (i, c) in chunks.iter().enumerate() {
            added += tx.execute(
                "INSERT OR IGNORE INTO research_chunks(url, ord, text, chunk_hash) VALUES (?1, ?2, ?3, ?4)",
                params![url, i as i64, c, content_hash(c)],
            )?;
        }
        tx.commit()?;
        Ok(added)
    }

    pub fn cached_page(&self, url: &str, max_age_secs: i64) -> Result<Option<CachedPage>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT url, title, text, content_hash, fetched_at FROM research_pages WHERE url = ?1 AND fetched_at >= ?2",
                params![url, now() - max_age_secs],
                |r| {
                    Ok(CachedPage {
                        url: r.get(0)?,
                        title: r.get(1)?,
                        text: r.get(2)?,
                        content_hash: r.get(3)?,
                        fetched_at: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Sucht in bereits recherchiertem Wissen.
    pub fn search_knowledge(&self, query: &str, max_age_secs: i64, limit: usize) -> Result<Vec<KnowledgeHit>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare(
            "SELECT c.url, p.title, c.text, research_chunks_fts.rank
             FROM research_chunks_fts
             JOIN research_chunks c ON c.id = research_chunks_fts.rowid
             JOIN research_pages p ON p.url = c.url
             WHERE research_chunks_fts MATCH ?1 AND p.fetched_at >= ?2
             ORDER BY research_chunks_fts.rank LIMIT ?3",
        )?;
        let rows = st.query_map(params![q, now() - max_age_secs, limit as i64], |r| {
            Ok(KnowledgeHit { url: r.get(0)?, title: r.get(1)?, text: r.get(2)?, rank: r.get(3)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Entfernt veraltete Cache-Einträge.
    pub fn prune_research(&self, max_age_secs: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = now() - max_age_secs;
        let a = conn.execute("DELETE FROM research_queries WHERE fetched_at < ?1", [cutoff])?;
        let b = conn.execute("DELETE FROM research_pages WHERE fetched_at < ?1", [cutoff])?;
        Ok(a + b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_normalization_reuses_cache() {
        let m = Memory::in_memory().unwrap();
        m.cache_search("Wetter Wien morgen", "brave", "[1]").unwrap();
        assert_eq!(m.cached_search("morgen wetter WIEN!", "brave", 3600).unwrap().as_deref(), Some("[1]"));
        assert!(m.cached_search("morgen wetter wien", "ddg", 3600).unwrap().is_none());
        assert!(m.cached_search("morgen wetter wien", "brave", -10).unwrap().is_none(), "abgelaufen");
    }

    #[test]
    fn pages_chunks_dedup_and_knowledge_search() {
        let m = Memory::in_memory().unwrap();
        let chunks = vec!["Tauri ist ein Framework für Desktop-Apps.".to_string(), "Es nutzt Rust im Backend.".to_string()];
        assert_eq!(m.cache_page("https://a.example/t", "Tauri", &chunks.join("\n"), &chunks).unwrap(), 2);
        // Zweite Seite mit einem identischen Abschnitt → nur der neue wird gespeichert.
        let chunks2 = vec!["Es  nutzt Rust im   Backend.".to_string(), "React rendert die Oberfläche.".to_string()];
        assert_eq!(m.cache_page("https://b.example/t", "Tauri 2", &chunks2.join("\n"), &chunks2).unwrap(), 1);

        let hits = m.search_knowledge("Rust Backend", 3600, 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://a.example/t");
        assert!(m.cached_page("https://a.example/t", 3600).unwrap().is_some());
        // Neu laden ersetzt Abschnitte statt sie zu verdoppeln.
        assert_eq!(m.cache_page("https://a.example/t", "Tauri", &chunks.join("\n"), &chunks).unwrap(), 2);
        assert_eq!(m.search_knowledge("Framework", 3600, 5).unwrap().len(), 1);
    }

    #[test]
    fn fts_query_is_injection_safe() {
        assert_eq!(fts_query("a OR \"b\" NEAR(c)"), "\"or\" OR \"near\"");
        let m = Memory::in_memory().unwrap();
        assert!(m.search_knowledge("\" ) DROP TABLE x; --", 3600, 5).is_ok());
    }
}
