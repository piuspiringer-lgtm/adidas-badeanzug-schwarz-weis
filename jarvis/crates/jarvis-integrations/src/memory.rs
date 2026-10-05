//! Memory-Tools: Der Agent kann sich auf ausdrücklichen Wunsch etwas merken
//! und Gemerktes abrufen. `memory_remember` bietet der Agent nur an, wenn
//! der Benutzer ausdrücklich darum bittet (Schutz vor "Memory-Poisoning"
//! durch Inhalte aus Mails/Webseiten).

use async_trait::async_trait;
use jarvis_memory::Memory;
use jarvis_permissions::{Access, CallFacts, Capability, Confirmation, Integration, RiskLevel, ToolSpec};
use jarvis_runtime::{Tool, ToolError, ToolOutput};
use serde_json::{json, Value};
use std::sync::Arc;

static REMEMBER: ToolSpec = ToolSpec {
    name: "memory_remember",
    description: "Merkt sich eine Information oder Präferenz des Benutzers dauerhaft (nur auf ausdrücklichen Wunsch).",
    integration: Integration::Memory,
    access: Access::Write,
    risk: RiskLevel::Low,
    confirmation: Confirmation::Never,
    capabilities: &[Capability::MemoryWrite],
    services: &[],
};

static RECALL: ToolSpec = ToolSpec {
    name: "memory_recall",
    description: "Sucht in gemerkten Informationen und Präferenzen.",
    integration: Integration::Memory,
    access: Access::Read,
    risk: RiskLevel::Low,
    confirmation: Confirmation::Never,
    capabilities: &[Capability::MemoryRead],
    services: &[],
};

pub struct Remember(pub Memory);
pub struct Recall(pub Memory);

fn s<'a>(a: &'a Value, k: &str) -> Result<&'a str, ToolError> {
    a.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).ok_or_else(|| ToolError::InvalidArgs(format!("'{k}' fehlt")))
}

#[async_trait]
impl Tool for Remember {
    fn spec(&self) -> &ToolSpec {
        &REMEMBER
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{
            "topic":{"type":"string","description":"kurzes Thema, z. B. 'lieblingsfarbe'"},
            "fact":{"type":"string"},
            "preference":{"type":"boolean","description":"true = dauerhafte Präferenz"}},"required":["topic","fact"]})
    }
    fn facts(&self, a: &Value) -> Result<CallFacts, ToolError> {
        Ok(CallFacts { preview: format!("Merken: {} = {}", s(a, "topic")?, s(a, "fact")?), ..Default::default() })
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let (topic, fact) = (s(&a, "topic")?, s(&a, "fact")?);
        if topic.chars().count() > 80 || fact.chars().count() > 500 {
            return Err(ToolError::InvalidArgs("zu lang".into()));
        }
        let e = |e: jarvis_memory::MemoryError| ToolError::Failed(e.to_string());
        if a.get("preference").and_then(Value::as_bool).unwrap_or(false) {
            self.0.set_preference(topic, fact).map_err(e)?;
        } else {
            self.0.remember_fact(topic, fact, "user").map_err(e)?;
        }
        Ok(ToolOutput::text(format!("Gemerkt: {topic} – {fact}")))
    }
}

#[async_trait]
impl Tool for Recall {
    fn spec(&self) -> &ToolSpec {
        &RECALL
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})
    }
    async fn call(&self, a: Value) -> Result<ToolOutput, ToolError> {
        let q = s(&a, "query")?.to_lowercase();
        let e = |e: jarvis_memory::MemoryError| ToolError::Failed(e.to_string());
        let mut lines: Vec<String> = self.0.search_facts(&q, 8).map_err(e)?.into_iter().map(|(t, f)| format!("- [{t}] {f}")).collect();
        for (k, v) in self.0.preferences(50).map_err(e)? {
            if q.split_whitespace().any(|w| k.to_lowercase().contains(w) || v.to_lowercase().contains(w)) {
                lines.push(format!("- Präferenz {k}: {v}"));
            }
        }
        Ok(ToolOutput::text(if lines.is_empty() { "Nichts dazu gespeichert.".into() } else { lines.join("\n") }))
    }
}

pub fn tools(memory: Memory) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(Remember(memory.clone())), Arc::new(Recall(memory))]
}
