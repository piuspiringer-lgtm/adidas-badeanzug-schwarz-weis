use async_trait::async_trait;
use jarvis_permissions::{validate_spec, CallFacts, PolicyViolation, ToolSpec};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("ungültige Argumente: {0}")]
    InvalidArgs(String),
    #[error("nicht gefunden: {0}")]
    NotFound(String),
    #[error("nicht erlaubt: {0}")]
    Forbidden(String),
    #[error("nicht konfiguriert: {0}")]
    NotConfigured(String),
    #[error("Fehler: {0}")]
    Failed(String),
}

/// Ergebnis eines Tool-Aufrufs. `text` ist das, was (ggf. gekürzt) ans
/// Modell geht; `data` sind strukturierte Daten für UI und Memory.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ToolOutput {
    pub text: String,
    pub data: Value,
}

impl ToolOutput {
    pub fn text(t: impl Into<String>) -> Self {
        Self { text: t.into(), data: Value::Null }
    }
    pub fn with_data(t: impl Into<String>, data: Value) -> Self {
        Self { text: t.into(), data }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;

    /// JSON-Schema der Argumente (für das LLM).
    fn parameters(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    /// Ermittelt vor der Ausführung, was der Aufruf konkret tun würde
    /// (Pfade, Überschreiben, Vorschau). Grundlage für Policy und Bestätigung.
    fn facts(&self, _args: &Value) -> Result<CallFacts, ToolError> {
        Ok(CallFacts::default())
    }

    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError>;
}

/// Registry aller Tools. Tools können nur über den Gateway aufgerufen werden:
/// Es gibt bewusst keine öffentliche Methode, die ein Tool herausgibt.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<&'static str, Arc<dyn Tool>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolInfo {
    pub spec: ToolSpec,
    pub parameters: Value,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registriert ein Tool, nachdem die Policy es geprüft hat.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), PolicyViolation> {
        let spec = tool.spec();
        validate_spec(spec)?;
        if self.tools.contains_key(spec.name) {
            return Err(PolicyViolation::InconsistentSpec {
                tool: spec.name.into(),
                reason: "Name bereits registriert".into(),
            });
        }
        self.tools.insert(spec.name, tool);
        Ok(())
    }

    pub fn list(&self) -> Vec<ToolInfo> {
        self.tools
            .values()
            .map(|t| ToolInfo { spec: t.spec().clone(), parameters: t.parameters() })
            .collect()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.keys().copied().collect()
    }

    pub(crate) fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }
}
