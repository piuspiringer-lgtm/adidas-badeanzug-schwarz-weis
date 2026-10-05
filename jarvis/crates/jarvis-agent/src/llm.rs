//! Schnittstelle zum Sprachmodell. Produktiv: [`ModelManager`] (Ollama).
//! In Tests: ein skriptgesteuertes Fake-Modell.

use async_trait::async_trait;
use jarvis_context::{Message, ModelTier};
use jarvis_models::ModelManager;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LlmReply {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub model: String,
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    /// Länge des entfernten Denktexts (Zeichen) – nur zur Diagnose, nie sichtbar.
    pub hidden_reasoning_chars: usize,
    /// Antwort wurde durch die Längenbegrenzung abgeschnitten.
    pub truncated: bool,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, tier: ModelTier, messages: &[Message], tools: &[Value]) -> Result<LlmReply, String>;

    /// Größe des Kontextfensters des aktiven Profils.
    fn context_window(&self) -> usize {
        8192
    }

    /// Vom Resource Manager: kleines Modell erzwingen / keep_alive setzen.
    fn apply_power(&self, _force_small: bool, _keep_alive_secs: u64) {}

    /// Name des Modells, das für eine Stufe genutzt würde (für Anzeige).
    fn model_name(&self, _tier: ModelTier) -> String {
        String::new()
    }
}

/// Entfernt Denk-/Planungstext des Modells, damit er nie beim Benutzer landet:
/// * `<think>…</think>`-Blöcke,
/// * ein offenes `<think>` ohne Ende (alles danach),
/// * ein **einzelnes schließendes** `</think>` (alles davor). So liefern Qwen3-
///   "Thinking"-Varianten ihren Denktext, weil das öffnende Tag im Template steckt.
pub fn strip_thinking(s: &str) -> String {
    // Alles bis einschließlich des letzten schließenden Tags ist Denktext.
    let s = match s.rfind("</think>") {
        Some(i) => &s[i + "</think>".len()..],
        None => s,
    };
    // Ein danach noch offenes <think> (abgebrochene Generierung): Rest verwerfen.
    let s = match s.find("<think>") {
        Some(i) => &s[..i],
        None => s,
    };
    s.trim().to_string()
}

fn parse_args(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Object(Default::default())),
        Value::Null => Value::Object(Default::default()),
        v => v.clone(),
    }
}

/// Liest Tool-Aufrufe aus der Ollama-Antwort. Fallback: `<tool_call>{…}</tool_call>`
/// im Text (manche Modelle/Templates liefern das statt nativer Aufrufe).
pub fn parse_tool_calls(native: &[Value], content: &str) -> (Vec<ToolCall>, String) {
    let mut calls: Vec<ToolCall> = native
        .iter()
        .filter_map(|c| {
            let f = c.get("function").unwrap_or(c);
            let name = f.get("name")?.as_str()?.to_string();
            Some(ToolCall { name, arguments: parse_args(f.get("arguments").unwrap_or(&Value::Null)) })
        })
        .collect();
    let mut text = strip_thinking(content);
    if calls.is_empty() {
        while let Some(start) = text.find("<tool_call>") {
            let Some(end_rel) = text[start..].find("</tool_call>") else { break };
            let inner = text[start + "<tool_call>".len()..start + end_rel].trim().to_string();
            if let Ok(v) = serde_json::from_str::<Value>(&inner) {
                if let Some(name) = v.get("name").and_then(Value::as_str) {
                    calls.push(ToolCall { name: name.to_string(), arguments: parse_args(v.get("arguments").unwrap_or(&Value::Null)) });
                }
            }
            text.replace_range(start..start + end_rel + "</tool_call>".len(), "");
        }
        text = text.trim().to_string();
    }
    (calls, text)
}

#[async_trait]
impl LlmClient for ModelManager {
    async fn chat(&self, tier: ModelTier, messages: &[Message], tools: &[Value]) -> Result<LlmReply, String> {
        let r = ModelManager::chat(self, tier, messages, tools).await.map_err(|e| e.to_string())?;
        let (tool_calls, content) = parse_tool_calls(&r.tool_calls, &r.content);
        let hidden_reasoning_chars = r.content.chars().count().saturating_sub(content.chars().count());
        Ok(LlmReply {
            content,
            tool_calls,
            model: r.model,
            prompt_tokens: r.prompt_tokens,
            output_tokens: r.output_tokens,
            hidden_reasoning_chars,
            truncated: r.truncated,
        })
    }

    fn context_window(&self) -> usize {
        self.profile().context_window
    }

    fn apply_power(&self, force_small: bool, keep_alive_secs: u64) {
        self.set_force_fallback(force_small, keep_alive_secs);
    }

    fn model_name(&self, tier: ModelTier) -> String {
        self.model_for(tier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_native_and_text_tool_calls() {
        let native = vec![json!({"function": {"name": "fs_list", "arguments": {"path": "~/Documents"}}})];
        let (c, _) = parse_tool_calls(&native, "");
        assert_eq!(c, vec![ToolCall { name: "fs_list".into(), arguments: json!({"path": "~/Documents"}) }]);

        let native = vec![json!({"function": {"name": "fs_read", "arguments": "{\"path\": \"a.txt\"}"}})];
        assert_eq!(parse_tool_calls(&native, "").0[0].arguments, json!({"path": "a.txt"}));

        let text = "<think>überlegen</think>Ich schaue nach.\n<tool_call>\n{\"name\": \"fs_search\", \"arguments\": {\"pattern\": \"*.pdf\"}}\n</tool_call>";
        let (c, rest) = parse_tool_calls(&[], text);
        assert_eq!(c[0].name, "fs_search");
        assert_eq!(rest, "Ich schaue nach.");
    }

    #[test]
    fn thinking_is_removed() {
        assert_eq!(strip_thinking("<think>a</think>\nAntwort"), "Antwort");
        assert_eq!(strip_thinking("Antwort<think>unvollständig"), "Antwort");
        assert_eq!(strip_thinking("<think>a</think>X<think>b</think>Y"), "Y");
        assert_eq!(strip_thinking("Ohne Denktext."), "Ohne Denktext.");
    }

    /// Regression (Mac-Test): Qwen3-Thinking liefert nur das schließende Tag.
    #[test]
    fn leaked_reasoning_before_closing_tag_is_removed() {
        let raw = "Okay, let's see. The user asked to find PDFs in the Documents folder. \
                   I called fs_search and it returned two files, so I should list them in German.\n</think>\n\n\
                   Ich habe 2 PDFs in Dokumente gefunden: Rechnung.pdf und Zeugnis.pdf.";
        let out = strip_thinking(raw);
        assert_eq!(out, "Ich habe 2 PDFs in Dokumente gefunden: Rechnung.pdf und Zeugnis.pdf.");
        assert!(!out.contains("let's see"));
        let (calls, text) = parse_tool_calls(&[], raw);
        assert!(calls.is_empty());
        assert!(text.starts_with("Ich habe 2 PDFs"));
    }
}
