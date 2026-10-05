//! Context/Token Efficiency Manager.
//!
//! Ziel ist echte Effizienz: weniger, aber relevantere Informationen an das
//! Modell geben. Es werden keinerlei Nutzungs- oder Abrechnungsgrenzen
//! externer Anbieter umgangen; dieses Modul entscheidet nur, *was* in den
//! Prompt kommt und *welches* (meist lokale) Modell eine Aufgabe bekommt.

pub mod rank;
pub mod router;

pub use rank::{dedupe, is_near_duplicate, rank_chunks, Snippet};
pub use router::{Complexity, ModelTier, Router};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Grobe, konservative Token-Schätzung (deutsche Texte ≈ 3,5 Zeichen/Token).
pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    chars.div_ceil(7) * 2
}

/// Kürzt einen Text auf ca. `max_tokens`, behält Anfang (70 %) und Ende (30 %).
pub fn truncate_middle(text: &str, max_tokens: usize) -> String {
    if estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }
    let max_chars = max_tokens * 7 / 2;
    let chars: Vec<char> = text.chars().collect();
    let head = max_chars * 7 / 10;
    let tail = max_chars.saturating_sub(head);
    let removed = chars.len() - head - tail;
    format!(
        "{}\n[… {removed} Zeichen gekürzt …]\n{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    )
}

/// Teilt einen Text an Absatz-/Satzgrenzen in Abschnitte von ca. `target_tokens`.
pub fn chunk_text(text: &str, target_tokens: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    // (Text, beginnt einen neuen Absatz)
    let pieces = text
        .split("\n\n")
        .flat_map(|p| {
            if estimate_tokens(p) > target_tokens {
                p.split_inclusive(['.', '!', '?']).enumerate().map(|(i, s)| (s.to_string(), i == 0)).collect::<Vec<_>>()
            } else {
                vec![(p.to_string(), true)]
            }
        })
        .map(|(p, start)| (p.split_whitespace().collect::<Vec<_>>().join(" "), start))
        .filter(|(p, _)| !p.is_empty());
    for (p, para_start) in pieces {
        let cur_t = estimate_tokens(&cur);
        // Absatzgrenzen respektieren, sobald der aktuelle Abschnitt halb voll ist.
        if !cur.is_empty() && (cur_t + estimate_tokens(&p) > target_tokens || (para_start && cur_t >= target_tokens / 2)) {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&p);
        if estimate_tokens(&cur) > target_tokens * 2 {
            out.push(truncate_middle(&std::mem::take(&mut cur), target_tokens * 2));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Kompaktiert Tool-Ausgaben: JSON-Arrays werden auf die ersten Einträge
/// reduziert (mit Hinweis auf die Gesamtzahl), sonstiger Text wird gekürzt.
pub fn compact_tool_output(text: &str, max_tokens: usize) -> String {
    if estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }
    if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(text) {
        let total = items.len();
        let mut kept = Vec::new();
        for it in items {
            let candidate = serde_json::to_string(&kept.iter().chain([&it]).collect::<Vec<_>>()).unwrap();
            if estimate_tokens(&candidate) > max_tokens.saturating_sub(20) {
                break;
            }
            kept.push(it);
        }
        return format!(
            "{}\n[{} von {total} Einträgen gezeigt]",
            serde_json::to_string(&kept).unwrap(),
            kept.len()
        );
    }
    truncate_middle(text, max_tokens)
}

/// Fasst Text zusammen – implementiert vom (lokalen) Modell-Manager.
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, text: &str, max_tokens: usize) -> Result<String, String>;
}

/// Fasst lange Inhalte per Map-Reduce zusammen – aber nur, wenn sie das
/// Budget überschreiten. Kurze Inhalte werden unverändert durchgereicht.
pub async fn summarize_if_needed(text: &str, budget: usize, s: &dyn Summarizer) -> Result<String, String> {
    if estimate_tokens(text) <= budget {
        return Ok(text.to_string());
    }
    let chunks = chunk_text(text, 1500);
    let per = (budget / chunks.len().max(1)).max(60);
    let mut parts = Vec::with_capacity(chunks.len());
    for c in &chunks {
        parts.push(s.summarize(c, per).await?);
    }
    let joined = parts.join("\n");
    if estimate_tokens(&joined) <= budget {
        Ok(joined)
    } else {
        s.summarize(&joined, budget).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    /// Tool-Aufrufe einer Assistenten-Nachricht (Ollama-Format).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<serde_json::Value>,
    /// Name des Tools bei `role = "tool"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl Message {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self { role: role.into(), content: content.into(), tool_calls: vec![], tool_name: None }
    }

    pub fn assistant_tool_calls(content: impl Into<String>, calls: Vec<serde_json::Value>) -> Self {
        Self { role: "assistant".into(), content: content.into(), tool_calls: calls, tool_name: None }
    }

    pub fn tool(name: &str, content: impl Into<String>) -> Self {
        Self { role: "tool".into(), content: content.into(), tool_calls: vec![], tool_name: Some(name.into()) }
    }
}

/// Aufteilung des Kontextfensters.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Budget {
    pub window: usize,
    pub system: usize,
    pub history: usize,
    pub tools: usize,
    pub answer: usize,
}

impl Budget {
    pub fn for_window(window: usize) -> Self {
        Self {
            window,
            system: window / 10,
            history: window * 3 / 10,
            tools: window * 45 / 100,
            answer: window * 15 / 100,
        }
    }
}

/// Komprimiert den Gesprächsverlauf: System-Nachrichten und die letzten
/// `keep_recent` Nachrichten bleiben wörtlich, ältere werden zu einer
/// Zusammenfassung verdichtet (per Modell oder, ohne Modell, extraktiv).
pub async fn compress_history(
    messages: &[Message],
    budget: usize,
    keep_recent: usize,
    summarizer: Option<&dyn Summarizer>,
) -> Vec<Message> {
    let total: usize = messages.iter().map(|m| estimate_tokens(&m.content)).sum();
    if total <= budget {
        return messages.to_vec();
    }
    let (system, rest): (Vec<_>, Vec<_>) = messages.iter().cloned().partition(|m| m.role == "system");
    let mut split = rest.len().saturating_sub(keep_recent);
    // Nie zwischen Tool-Aufruf und Tool-Ergebnis schneiden.
    while split > 0 && rest[split].role == "tool" {
        split -= 1;
    }
    let (old, recent) = rest.split_at(split);
    let mut recent = recent.to_vec();
    let recent_tokens: usize = recent.iter().map(|m| estimate_tokens(&m.content)).sum();
    let summary_budget = budget.saturating_sub(recent_tokens).max(80);

    let transcript: String = old.iter().map(|m| format!("{}: {}\n", m.role, m.content)).collect();
    let summary = match summarizer {
        Some(s) if !old.is_empty() => s
            .summarize(&transcript, summary_budget)
            .await
            .unwrap_or_else(|_| extractive_summary(old, summary_budget)),
        _ => extractive_summary(old, summary_budget),
    };
    let mut out = system;
    if !old.is_empty() {
        out.push(Message::new("system", format!("Zusammenfassung des bisherigen Gesprächs:\n{summary}")));
    }
    // Falls selbst die letzten Nachrichten zu groß sind: einzeln kürzen.
    let per = (budget / recent.len().max(1)).max(60);
    for m in &mut recent {
        m.content = truncate_middle(&m.content, per);
    }
    out.extend(recent);
    out
}

fn extractive_summary(old: &[Message], budget: usize) -> String {
    let mut s = String::new();
    for m in old {
        let first: String = m.content.split_inclusive(['.', '!', '?', '\n']).next().unwrap_or("").trim().to_string();
        let line = format!("- {}: {}\n", m.role, first);
        if estimate_tokens(&s) + estimate_tokens(&line) > budget {
            break;
        }
        s.push_str(&line);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSummarizer;
    #[async_trait]
    impl Summarizer for FakeSummarizer {
        async fn summarize(&self, text: &str, max_tokens: usize) -> Result<String, String> {
            Ok(truncate_middle(&format!("ZUSAMMENFASSUNG: {}", text.chars().take(40).collect::<String>()), max_tokens))
        }
    }

    #[test]
    fn token_estimate_is_reasonable() {
        assert_eq!(estimate_tokens(""), 0);
        let t = "Das ist ein ganz normaler deutscher Satz mit einigen Wörtern.";
        let e = estimate_tokens(t);
        assert!((12..=25).contains(&e), "{e}");
    }

    #[test]
    fn truncation_respects_budget() {
        let long = "a".repeat(10_000);
        let t = truncate_middle(&long, 200);
        assert!(estimate_tokens(&t) <= 230);
        assert!(t.contains("gekürzt"));
        assert_eq!(truncate_middle("kurz", 200), "kurz");
    }

    #[test]
    fn chunks_stay_near_target() {
        let text = (0..200).map(|i| format!("Satz Nummer {i} handelt von etwas.")).collect::<Vec<_>>().join(" ");
        let chunks = chunk_text(&text, 100);
        assert!(chunks.len() > 5);
        assert!(chunks.iter().all(|c| estimate_tokens(c) <= 220));
    }

    #[test]
    fn json_arrays_are_compacted_not_cut_mid_item() {
        let items: Vec<_> = (0..500).map(|i| serde_json::json!({"id": i, "name": format!("Datei {i}.txt")})).collect();
        let out = compact_tool_output(&serde_json::to_string(&items).unwrap(), 300);
        assert!(out.contains("von 500 Einträgen"));
        let json_part = out.lines().next().unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(json_part).is_ok());
    }

    #[tokio::test]
    async fn summarize_only_when_needed() {
        assert_eq!(summarize_if_needed("kurz", 100, &FakeSummarizer).await.unwrap(), "kurz");
        let long = "Wort ".repeat(5000);
        let s = summarize_if_needed(&long, 300, &FakeSummarizer).await.unwrap();
        assert!(estimate_tokens(&s) <= 330);
    }

    #[tokio::test]
    async fn history_compression_keeps_recent_and_system() {
        let mut msgs = vec![Message::new("system", "Du bist JARVIS.")];
        for i in 0..40 {
            msgs.push(Message::new("user", format!("Frage {i}: {}", "bla ".repeat(50))));
            msgs.push(Message::new("assistant", format!("Antwort {i}. {}", "blub ".repeat(50))));
        }
        let out = compress_history(&msgs, 800, 4, Some(&FakeSummarizer)).await;
        assert_eq!(out[0].content, "Du bist JARVIS.");
        assert!(out[1].content.starts_with("Zusammenfassung"));
        assert_eq!(out.len(), 2 + 4);
        assert!(out.last().unwrap().content.starts_with("Antwort 39"));
        let total: usize = out.iter().map(|m| estimate_tokens(&m.content)).sum();
        assert!(total < 1200, "{total}");

        let out2 = compress_history(&msgs, 800, 4, None).await;
        assert!(out2[1].content.contains("Frage 0"));
    }

    #[tokio::test]
    async fn history_compression_keeps_tool_results_with_their_call() {
        let mut msgs = vec![Message::new("system", "S")];
        for i in 0..20 {
            msgs.push(Message::new("user", format!("Frage {i} {}", "x ".repeat(80))));
        }
        msgs.push(Message::assistant_tool_calls("", vec![serde_json::json!({"function": {"name": "fs_list"}})]));
        msgs.push(Message::tool("fs_list", "a.txt"));
        msgs.push(Message::tool("fs_list", "b.txt"));
        let out = compress_history(&msgs, 300, 2, None).await;
        let first_recent = out.iter().position(|m| m.role != "system").unwrap();
        assert_eq!(out[first_recent].role, "assistant", "Tool-Ergebnisse ohne Aufruf: {out:?}");
        assert_eq!(out.last().unwrap().tool_name.as_deref(), Some("fs_list"));
    }

    #[test]
    fn budget_split_sums_to_window() {
        let b = Budget::for_window(8192);
        assert!(b.system + b.history + b.tools + b.answer <= 8192);
    }
}
