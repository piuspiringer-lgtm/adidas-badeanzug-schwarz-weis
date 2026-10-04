//! Relevanz-Ranking (BM25) und Duplikaterkennung für Abschnitte.

use crate::estimate_tokens;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

const STOPWORDS: &[&str] = &[
    "der", "die", "das", "und", "oder", "ein", "eine", "einer", "eines", "ist", "sind", "war", "in", "im", "zu", "zum",
    "zur", "mit", "von", "vom", "auf", "für", "an", "am", "als", "auch", "es", "sie", "er", "wir", "ich", "du", "nicht",
    "den", "dem", "des", "wie", "was", "bei", "aus", "the", "a", "an", "and", "or", "of", "to", "in", "is", "are", "for",
    "on", "with", "as", "by", "it", "this", "that",
];

pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Snippet {
    pub source: String,
    pub text: String,
    pub score: f64,
}

fn shingles(text: &str) -> HashSet<String> {
    let w: Vec<String> = text.split_whitespace().map(|s| s.to_lowercase()).collect();
    if w.len() < 3 {
        return w.into_iter().collect();
    }
    w.windows(3).map(|s| s.join(" ")).collect()
}

/// Nahezu identische Texte (Jaccard der Wort-3-Gramme ≥ 0,8).
pub fn is_near_duplicate(a: &str, b: &str) -> bool {
    let (sa, sb) = (shingles(a), shingles(b));
    if sa.is_empty() || sb.is_empty() {
        return sa == sb;
    }
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    inter / union >= 0.8
}

/// Entfernt Duplikate (behält jeweils das erste Vorkommen).
pub fn dedupe(snippets: Vec<Snippet>) -> Vec<Snippet> {
    let mut out: Vec<Snippet> = Vec::new();
    for s in snippets {
        if !out.iter().any(|o| is_near_duplicate(&o.text, &s.text)) {
            out.push(s);
        }
    }
    out
}

/// Bewertet `chunks` (Quelle, Text) per BM25 gegen `query`, entfernt Duplikate
/// und gibt die besten Abschnitte zurück, bis `budget_tokens` erreicht ist.
pub fn rank_chunks(query: &str, chunks: &[(String, String)], budget_tokens: usize) -> Vec<Snippet> {
    let q: HashSet<String> = tokenize(query).into_iter().collect();
    if q.is_empty() || chunks.is_empty() {
        return vec![];
    }
    let docs: Vec<Vec<String>> = chunks.iter().map(|(_, t)| tokenize(t)).collect();
    let n = docs.len() as f64;
    let avgdl = docs.iter().map(Vec::len).sum::<usize>() as f64 / n;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for d in &docs {
        for t in d.iter().collect::<HashSet<_>>() {
            if q.contains(t) {
                *df.entry(t.as_str()).or_default() += 1;
            }
        }
    }
    let (k1, b) = (1.2, 0.75);
    let mut scored: Vec<Snippet> = chunks
        .iter()
        .zip(&docs)
        .map(|((src, text), d)| {
            let mut tf: HashMap<&str, usize> = HashMap::new();
            for t in d {
                if q.contains(t) {
                    *tf.entry(t.as_str()).or_default() += 1;
                }
            }
            let dl = d.len() as f64;
            let score = tf
                .iter()
                .map(|(t, f)| {
                    let df = df[t] as f64;
                    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                    let f = *f as f64;
                    idf * f * (k1 + 1.0) / (f + k1 * (1.0 - b + b * dl / avgdl.max(1.0)))
                })
                .sum();
            Snippet { source: src.clone(), text: text.clone(), score }
        })
        .filter(|s| s.score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    let mut out = Vec::new();
    let mut used = 0;
    for s in dedupe(scored) {
        let t = estimate_tokens(&s.text);
        if used + t > budget_tokens {
            continue;
        }
        used += t;
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(src: &str, t: &str) -> (String, String) {
        (src.into(), t.into())
    }

    #[test]
    fn ranks_relevant_first_and_drops_irrelevant() {
        let chunks = vec![
            c("a", "Das Wetter in Wien ist heute sonnig."),
            c("b", "Ollama lädt Modelle in den Speicher und entlädt sie nach keep_alive."),
            c("c", "Rezepte für Apfelstrudel aus Wien."),
        ];
        let r = rank_chunks("Wie entlädt Ollama Modelle?", &chunks, 1000);
        assert_eq!(r[0].source, "b");
        assert!(r.iter().all(|s| s.source != "a"));
    }

    #[test]
    fn duplicates_removed_and_budget_enforced() {
        let t = "Tauri nutzt Rust im Backend und eine Webview für die Oberfläche der App.";
        let chunks = vec![c("a", t), c("b", &format!("{t} ")), c("c", "Tauri Rust Webview Oberfläche Größe klein.")];
        let r = rank_chunks("Tauri Rust Webview", &chunks, 1000);
        assert_eq!(r.len(), 2);
        let r = rank_chunks("Tauri Rust Webview", &chunks, 25);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn near_duplicate_detection() {
        assert!(is_near_duplicate("eins zwei drei vier fünf sechs", "Eins zwei drei vier fünf sechs"));
        assert!(!is_near_duplicate("eins zwei drei vier fünf sechs", "sieben acht neun zehn elf zwölf"));
    }
}
