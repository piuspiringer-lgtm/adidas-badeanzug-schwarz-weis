//! Der Tool-Gateway: einziger Weg, ein Tool auszuführen.
//!
//! Ablauf je Aufruf: Sperrliste → Tool suchen → Fakten ermitteln → Policy →
//! ggf. Bestätigung durch den Menschen → Dienste laden → Ausführen → Audit.
//! Jeder Schritt, auch eine Ablehnung, wird im Audit-Log festgehalten.

use crate::lifecycle::ServiceManager;
use crate::tool::{ToolError, ToolOutput, ToolRegistry};
use async_trait::async_trait;
use jarvis_permissions::{is_hard_denied, CallFacts, Decision, Origin, PolicyEngine, ToolSpec};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    pub tool: String,
    pub origin: Origin,
    pub args: Value,
    pub decision: String,
    pub outcome: String,
    pub duration_ms: u128,
}

/// Ziel für Audit-Ereignisse (SQLite mit Hash-Kette in `jarvis-memory`).
pub trait AuditSink: Send + Sync {
    fn record(&self, event: &AuditEvent);
}

/// Fragt den Menschen (UI-Dialog). Der Agent hat keinen Zugriff darauf.
#[async_trait]
pub trait Confirmer: Send + Sync {
    async fn confirm(&self, spec: &ToolSpec, facts: &CallFacts, reason: &str) -> bool;
}

/// Lehnt jede Bestätigung ab – sicherer Standard ohne UI (z. B. CLI, Tests).
pub struct DenyAllConfirmer;

#[async_trait]
impl Confirmer for DenyAllConfirmer {
    async fn confirm(&self, _: &ToolSpec, _: &CallFacts, _: &str) -> bool {
        false
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("Aktion '{0}' ist dauerhaft gesperrt")]
    HardDenied(String),
    #[error("unbekanntes Tool '{0}'")]
    UnknownTool(String),
    #[error("von der Policy abgelehnt: {0}")]
    Denied(String),
    #[error("vom Benutzer nicht bestätigt: {0}")]
    NotConfirmed(String),
    #[error("Dienst nicht verfügbar: {0}")]
    Service(String),
    #[error(transparent)]
    Tool(#[from] ToolError),
}

pub struct ToolGateway {
    registry: ToolRegistry,
    policy: PolicyEngine,
    services: ServiceManager,
    confirmer: Arc<dyn Confirmer>,
    audit: Arc<dyn AuditSink>,
}

impl ToolGateway {
    pub fn new(
        registry: ToolRegistry,
        policy: PolicyEngine,
        services: ServiceManager,
        confirmer: Arc<dyn Confirmer>,
        audit: Arc<dyn AuditSink>,
    ) -> Self {
        Self { registry, policy, services, confirmer, audit }
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn services(&self) -> &ServiceManager {
        &self.services
    }

    fn log(&self, tool: &str, origin: Origin, args: &Value, decision: &str, outcome: &str, start: Instant) {
        self.audit.record(&AuditEvent {
            tool: tool.to_string(),
            origin,
            args: args.clone(),
            decision: decision.to_string(),
            outcome: outcome.to_string(),
            duration_ms: start.elapsed().as_millis(),
        });
    }

    pub async fn invoke(&self, name: &str, args: Value, origin: Origin) -> Result<ToolOutput, GatewayError> {
        let start = Instant::now();
        if is_hard_denied(name) {
            self.log(name, origin, &args, "hard_denied", "blocked", start);
            return Err(GatewayError::HardDenied(name.to_string()));
        }
        let Some(tool) = self.registry.get(name) else {
            self.log(name, origin, &args, "unknown", "blocked", start);
            return Err(GatewayError::UnknownTool(name.to_string()));
        };
        let spec = tool.spec();
        let facts = match tool.facts(&args) {
            Ok(f) => f,
            Err(e) => {
                self.log(name, origin, &args, "invalid", &e.to_string(), start);
                return Err(e.into());
            }
        };
        match self.policy.evaluate(spec, &facts, origin) {
            Decision::Allow => {}
            Decision::Deny { reason } => {
                self.log(name, origin, &args, "denied", &reason, start);
                return Err(GatewayError::Denied(reason));
            }
            Decision::RequireConfirmation { reason } => {
                if !self.confirmer.confirm(spec, &facts, &reason).await {
                    self.log(name, origin, &args, "not_confirmed", &reason, start);
                    return Err(GatewayError::NotConfirmed(reason));
                }
                self.log(name, origin, &args, "confirmed", &reason, start);
            }
        }

        let mut guards = Vec::new();
        for s in spec.services {
            match self.services.acquire(s).await {
                Ok(g) => guards.push(g),
                Err(e) => {
                    self.log(name, origin, &args, "allowed", &format!("service_error: {e}"), start);
                    return Err(GatewayError::Service(e));
                }
            }
        }
        let result = tool.call(args.clone()).await;
        drop(guards);
        match &result {
            Ok(out) => self.log(name, origin, &args, "allowed", &format!("ok ({} Zeichen)", out.text.len()), start),
            Err(e) => self.log(name, origin, &args, "allowed", &format!("error: {e}"), start),
        }
        Ok(result?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Tool;
    use jarvis_permissions::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemAudit(Mutex<Vec<AuditEvent>>);
    impl AuditSink for MemAudit {
        fn record(&self, e: &AuditEvent) {
            self.0.lock().unwrap().push(e.clone());
        }
    }

    struct AllowAll;
    #[async_trait]
    impl Confirmer for AllowAll {
        async fn confirm(&self, _: &ToolSpec, _: &CallFacts, _: &str) -> bool {
            true
        }
    }

    struct Echo(ToolSpec);
    #[async_trait]
    impl Tool for Echo {
        fn spec(&self) -> &ToolSpec {
            &self.0
        }
        async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text(args.to_string()))
        }
    }

    fn echo(name: &'static str, access: Access) -> Arc<Echo> {
        Arc::new(Echo(ToolSpec {
            name,
            description: "echo",
            integration: Integration::System,
            access,
            risk: RiskLevel::Low,
            confirmation: if access == Access::Read { Confirmation::Never } else { Confirmation::Always },
            capabilities: &[],
            services: &[],
        }))
    }

    fn gateway(confirmer: Arc<dyn Confirmer>) -> (ToolGateway, Arc<MemAudit>) {
        let mut r = ToolRegistry::new();
        r.register(echo("echo_read", Access::Read)).unwrap();
        r.register(echo("echo_destroy", Access::Destructive)).unwrap();
        let audit = Arc::new(MemAudit::default());
        (ToolGateway::new(r, PolicyEngine::new(vec![]), ServiceManager::new(), confirmer, audit.clone()), audit)
    }

    #[tokio::test]
    async fn hard_denied_calls_are_blocked_and_audited() {
        let (g, audit) = gateway(Arc::new(AllowAll));
        for a in ["send_email", "delete_email", "send_teams_message", "edit_teams_message", "modify_webuntis"] {
            let r = g.invoke(a, serde_json::json!({"to": "x"}), Origin::Agent).await;
            assert!(matches!(r, Err(GatewayError::HardDenied(_))), "{a}");
        }
        let log = audit.0.lock().unwrap();
        assert_eq!(log.len(), 5);
        assert!(log.iter().all(|e| e.decision == "hard_denied"));
    }

    #[test]
    fn hard_denied_tools_cannot_be_registered() {
        let mut r = ToolRegistry::new();
        for a in ["send_email", "delete_email", "send_teams_message", "edit_teams_message", "modify_webuntis"] {
            assert!(r.register(echo(a, Access::Read)).is_err(), "{a}");
        }
        assert!(r.names().is_empty());
    }

    #[tokio::test]
    async fn destructive_needs_confirmation() {
        let (g, audit) = gateway(Arc::new(DenyAllConfirmer));
        let r = g.invoke("echo_destroy", Value::Null, Origin::Agent).await;
        assert!(matches!(r, Err(GatewayError::NotConfirmed(_))));
        assert_eq!(audit.0.lock().unwrap()[0].decision, "not_confirmed");

        let (g, _) = gateway(Arc::new(AllowAll));
        assert!(g.invoke("echo_destroy", Value::Null, Origin::User).await.is_ok());
    }

    #[tokio::test]
    async fn read_tool_runs_and_is_audited() {
        let (g, audit) = gateway(Arc::new(DenyAllConfirmer));
        let out = g.invoke("echo_read", serde_json::json!({"a": 1}), Origin::Agent).await.unwrap();
        assert_eq!(out.text, r#"{"a":1}"#);
        assert_eq!(audit.0.lock().unwrap()[0].decision, "allowed");
    }

    #[tokio::test]
    async fn unknown_tool_rejected() {
        let (g, _) = gateway(Arc::new(AllowAll));
        assert!(matches!(
            g.invoke("rm_rf", Value::Null, Origin::Agent).await,
            Err(GatewayError::UnknownTool(_))
        ));
    }
}
