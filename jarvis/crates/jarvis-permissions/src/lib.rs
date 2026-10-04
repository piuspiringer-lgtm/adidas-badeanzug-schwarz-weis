//! Zentrale Permission-Schicht von JARVIS.
//!
//! Alles in diesem Crate ist bewusst **statisch einkompiliert**: Es gibt keine
//! Konfigurationsdatei, keine Datenbanktabelle und kein Tool, über das die
//! Regeln zur Laufzeit verändert werden könnten. Jede Änderung erfordert eine
//! neue, vom Menschen gebaute Version der App.

use serde::Serialize;
use std::fmt;
use std::path::{Path, PathBuf};

/// Lese-/Schreibcharakter eines Tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Access {
    /// Liest nur, verändert nichts.
    Read,
    /// Erzeugt oder verändert Daten, ohne etwas zu entfernen.
    Write,
    /// Kann Daten entfernen oder überschreiben.
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Confirmation {
    /// Keine Rückfrage (nur für Read-Tools mit niedrigem Risiko zulässig).
    Never,
    /// Nur wenn der konkrete Aufruf als riskant erkannt wird (z. B. Überschreiben).
    WhenRisky,
    /// Immer nachfragen.
    Always,
}

/// Integration, zu der ein Tool gehört.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Integration {
    Filesystem,
    Web,
    Email,
    Teams,
    WebUntis,
    Memory,
    Model,
    Voice,
    System,
}

impl Integration {
    /// Integrationen, die per Design niemals schreiben dürfen.
    pub const fn is_read_only(self) -> bool {
        matches!(self, Integration::Email | Integration::Teams | Integration::WebUntis)
    }
}

/// Fein granulare Fähigkeiten, die ein Tool benötigt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Capability {
    FsRead,
    FsWrite,
    FsTrash,
    OpenWithSystem,
    NetworkGet,
    MailRead,
    TeamsRead,
    WebUntisRead,
    MemoryRead,
    MemoryWrite,
    ModelInference,
    Microphone,
    Speaker,
}

impl Capability {
    /// Fähigkeiten, die etwas verändern.
    pub const fn is_mutating(self) -> bool {
        matches!(
            self,
            Capability::FsWrite | Capability::FsTrash | Capability::OpenWithSystem | Capability::MemoryWrite
        )
    }
}

/// Statische Beschreibung eines Tools. Jedes Tool MUSS eine liefern.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub integration: Integration,
    pub access: Access,
    pub risk: RiskLevel,
    pub confirmation: Confirmation,
    pub capabilities: &'static [Capability],
    /// Dienste (Lifecycle), die für die Ausführung geladen sein müssen.
    pub services: &'static [&'static str],
}

/// Aktionen, die JARVIS unter keinen Umständen ausführen darf. Ein Tool mit
/// einem dieser Namen kann nicht registriert und nicht aufgerufen werden.
pub const HARD_DENIED_ACTIONS: &[&str] = &[
    // E-Mail
    "send_email",
    "reply_email",
    "forward_email",
    "delete_email",
    "edit_email",
    "move_email",
    "draft_email",
    "mark_email",
    // Microsoft Teams
    "send_teams_message",
    "reply_teams_message",
    "edit_teams_message",
    "delete_teams_message",
    "react_teams_message",
    // WebUntis
    "modify_webuntis",
    "create_webuntis_entry",
    "edit_webuntis_entry",
    "delete_webuntis_entry",
    // Selbstschutz
    "modify_permissions",
    "modify_policy",
    "grant_permission",
    "disable_audit",
    "delete_audit_log",
    // Dateisystem: endgültiges Löschen gibt es nicht, nur Papierkorb
    "fs_delete_permanent",
    "shell_exec",
];

/// Wortbestandteile, die in Tool-Namen von Read-only-Integrationen niemals
/// vorkommen dürfen (zusätzliche Absicherung gegen Umbenennungen).
pub const FORBIDDEN_VERBS_READ_ONLY: &[&str] = &[
    "send", "reply", "forward", "delete", "remove", "edit", "update", "modify", "create", "write", "move",
    "patch", "post", "draft", "mark", "react", "set", "upload",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyViolation {
    HardDenied(String),
    ReadOnlyIntegration { tool: String, reason: String },
    InconsistentSpec { tool: String, reason: String },
    ProtectedPath(PathBuf),
}

impl fmt::Display for PolicyViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolicyViolation::HardDenied(n) => write!(f, "Aktion '{n}' ist dauerhaft gesperrt"),
            PolicyViolation::ReadOnlyIntegration { tool, reason } => {
                write!(f, "Tool '{tool}' verletzt Read-only-Regel: {reason}")
            }
            PolicyViolation::InconsistentSpec { tool, reason } => write!(f, "Tool '{tool}' ungültig: {reason}"),
            PolicyViolation::ProtectedPath(p) => write!(f, "Pfad {} ist geschützt", p.display()),
        }
    }
}

impl std::error::Error for PolicyViolation {}

/// Prüft, ob ein Tool-Name auf der Sperrliste steht.
pub fn is_hard_denied(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    HARD_DENIED_ACTIONS.iter().any(|d| *d == n)
}

/// Validiert ein Tool vor der Registrierung.
pub fn validate_spec(spec: &ToolSpec) -> Result<(), PolicyViolation> {
    let name = spec.name.to_ascii_lowercase();
    if is_hard_denied(&name) {
        return Err(PolicyViolation::HardDenied(spec.name.to_string()));
    }
    if spec.name.is_empty() || !spec.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err(PolicyViolation::InconsistentSpec {
            tool: spec.name.into(),
            reason: "Name muss snake_case sein".into(),
        });
    }

    let mutating_caps = spec.capabilities.iter().any(|c| c.is_mutating());
    if spec.access == Access::Read && mutating_caps {
        return Err(PolicyViolation::InconsistentSpec {
            tool: spec.name.into(),
            reason: "Read-Tool deklariert verändernde Fähigkeiten".into(),
        });
    }
    if spec.access == Access::Destructive && spec.confirmation != Confirmation::Always {
        return Err(PolicyViolation::InconsistentSpec {
            tool: spec.name.into(),
            reason: "destruktive Tools brauchen immer eine Bestätigung".into(),
        });
    }
    if spec.access != Access::Read && spec.confirmation == Confirmation::Never && spec.integration == Integration::Filesystem {
        return Err(PolicyViolation::InconsistentSpec {
            tool: spec.name.into(),
            reason: "verändernde Dateisystem-Tools brauchen eine Bestätigungsregel".into(),
        });
    }

    if spec.integration.is_read_only() {
        if spec.access != Access::Read {
            return Err(PolicyViolation::ReadOnlyIntegration {
                tool: spec.name.into(),
                reason: format!("{:?} ist read-only, Tool deklariert {:?}", spec.integration, spec.access),
            });
        }
        if mutating_caps {
            return Err(PolicyViolation::ReadOnlyIntegration {
                tool: spec.name.into(),
                reason: "verändernde Fähigkeit in Read-only-Integration".into(),
            });
        }
        if let Some(v) = name.split('_').find(|part| FORBIDDEN_VERBS_READ_ONLY.contains(part)) {
            return Err(PolicyViolation::ReadOnlyIntegration {
                tool: spec.name.into(),
                reason: format!("verbotenes Verb '{v}' im Namen"),
            });
        }
        let allowed: &[Capability] = match spec.integration {
            Integration::Email => &[Capability::MailRead, Capability::NetworkGet, Capability::ModelInference],
            Integration::Teams => &[Capability::TeamsRead, Capability::NetworkGet, Capability::ModelInference],
            Integration::WebUntis => &[Capability::WebUntisRead, Capability::NetworkGet, Capability::ModelInference],
            _ => unreachable!(),
        };
        if let Some(c) = spec.capabilities.iter().find(|c| !allowed.contains(c)) {
            return Err(PolicyViolation::ReadOnlyIntegration {
                tool: spec.name.into(),
                reason: format!("Fähigkeit {c:?} nicht erlaubt"),
            });
        }
    }
    Ok(())
}

/// Wer eine Aktion ausgelöst hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Origin {
    /// Direkt durch eine Benutzeraktion in der UI.
    User,
    /// Durch das LLM / den Agenten geplant.
    Agent,
    /// Durch einen gespeicherten Skill/Workflow.
    Skill,
}

/// Konkrete Informationen zu einem Aufruf, die die Policy beurteilt.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CallFacts {
    /// Dateipfade, die gelesen oder verändert werden.
    pub paths: Vec<PathBuf>,
    /// Ob bestehende Daten überschrieben würden.
    pub overwrites: bool,
    /// Menschlich lesbare Vorschau der Aktion (für Bestätigung und Audit).
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Decision {
    Allow,
    RequireConfirmation { reason: String },
    Deny { reason: String },
}

/// Policy-Engine. Hält nur unveränderliche Daten (geschützte Pfade), die beim
/// Start der App vom Host gesetzt werden – nie vom Agenten.
#[derive(Debug, Clone)]
pub struct PolicyEngine {
    protected_paths: Vec<PathBuf>,
}

impl PolicyEngine {
    pub fn new(protected_paths: Vec<PathBuf>) -> Self {
        Self { protected_paths }
    }

    pub fn protected_paths(&self) -> &[PathBuf] {
        &self.protected_paths
    }

    pub fn is_protected(&self, path: &Path) -> bool {
        self.protected_paths.iter().any(|p| path.starts_with(p))
    }

    pub fn evaluate(&self, spec: &ToolSpec, facts: &CallFacts, origin: Origin) -> Decision {
        if let Err(v) = validate_spec(spec) {
            return Decision::Deny { reason: v.to_string() };
        }
        if spec.access != Access::Read || spec.capabilities.contains(&Capability::FsRead) {
            if let Some(p) = facts.paths.iter().find(|p| self.is_protected(p)) {
                return Decision::Deny { reason: PolicyViolation::ProtectedPath(p.clone()).to_string() };
            }
        }
        let needs_confirm = match spec.confirmation {
            Confirmation::Always => true,
            Confirmation::WhenRisky => facts.overwrites || spec.risk >= RiskLevel::High,
            Confirmation::Never => false,
        } || spec.access == Access::Destructive
            || (origin != Origin::User && spec.risk >= RiskLevel::High);
        if needs_confirm {
            let reason = if facts.preview.is_empty() {
                format!("'{}' ({:?}, Risiko {:?})", spec.name, spec.access, spec.risk)
            } else {
                facts.preview.clone()
            };
            Decision::RequireConfirmation { reason }
        } else {
            Decision::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &'static str, integration: Integration, access: Access, caps: &'static [Capability]) -> ToolSpec {
        ToolSpec {
            name,
            description: "test",
            integration,
            access,
            risk: RiskLevel::Low,
            confirmation: if access == Access::Read { Confirmation::Never } else { Confirmation::Always },
            capabilities: caps,
            services: &[],
        }
    }

    #[test]
    fn required_actions_are_hard_denied() {
        for a in ["send_email", "delete_email", "send_teams_message", "edit_teams_message", "modify_webuntis"] {
            assert!(is_hard_denied(a), "{a}");
            assert!(is_hard_denied(&a.to_uppercase()), "{a} (Großschreibung)");
        }
    }

    #[test]
    fn hard_denied_spec_cannot_be_validated_even_if_declared_read() {
        for name in ["send_email", "delete_email", "send_teams_message", "edit_teams_message", "modify_webuntis"] {
            let s = spec(name, Integration::System, Access::Read, &[]);
            assert!(matches!(validate_spec(&s), Err(PolicyViolation::HardDenied(_))));
        }
    }

    #[test]
    fn read_only_integrations_reject_write_access() {
        for i in [Integration::Email, Integration::Teams, Integration::WebUntis] {
            let s = spec("foo_read", i, Access::Write, &[]);
            assert!(matches!(validate_spec(&s), Err(PolicyViolation::ReadOnlyIntegration { .. })));
        }
    }

    #[test]
    fn read_only_integrations_reject_mutating_verbs_and_caps() {
        let s = spec("email_send_copy", Integration::Email, Access::Read, &[Capability::MailRead]);
        assert!(validate_spec(&s).is_err());
        let s = spec("teams_post", Integration::Teams, Access::Read, &[Capability::TeamsRead]);
        assert!(validate_spec(&s).is_err());
        let s = spec("email_search", Integration::Email, Access::Read, &[Capability::FsWrite]);
        assert!(validate_spec(&s).is_err());
        let s = spec("email_search", Integration::Email, Access::Read, &[Capability::TeamsRead]);
        assert!(validate_spec(&s).is_err());
        let s = spec("email_search", Integration::Email, Access::Read, &[Capability::MailRead, Capability::NetworkGet]);
        assert!(validate_spec(&s).is_ok());
    }

    #[test]
    fn destructive_requires_always_confirmation() {
        let mut s = spec("fs_trash", Integration::Filesystem, Access::Destructive, &[Capability::FsTrash]);
        s.confirmation = Confirmation::WhenRisky;
        assert!(validate_spec(&s).is_err());
        s.confirmation = Confirmation::Always;
        assert!(validate_spec(&s).is_ok());
        let engine = PolicyEngine::new(vec![]);
        assert!(matches!(
            engine.evaluate(&s, &CallFacts::default(), Origin::User),
            Decision::RequireConfirmation { .. }
        ));
    }

    #[test]
    fn protected_paths_are_denied() {
        let engine = PolicyEngine::new(vec![PathBuf::from("/app/policy")]);
        let s = spec("fs_create", Integration::Filesystem, Access::Write, &[Capability::FsWrite]);
        let facts = CallFacts { paths: vec![PathBuf::from("/app/policy/grants.db")], ..Default::default() };
        assert!(matches!(engine.evaluate(&s, &facts, Origin::User), Decision::Deny { .. }));
        let r = spec("fs_read", Integration::Filesystem, Access::Read, &[Capability::FsRead]);
        assert!(matches!(engine.evaluate(&r, &facts, Origin::Agent), Decision::Deny { .. }));
    }

    #[test]
    fn read_tools_allowed_without_confirmation() {
        let engine = PolicyEngine::new(vec![]);
        let s = spec("web_search", Integration::Web, Access::Read, &[Capability::NetworkGet]);
        assert_eq!(engine.evaluate(&s, &CallFacts::default(), Origin::Agent), Decision::Allow);
    }

    #[test]
    fn overwrite_triggers_confirmation() {
        let engine = PolicyEngine::new(vec![]);
        let mut s = spec("fs_create", Integration::Filesystem, Access::Write, &[Capability::FsWrite]);
        s.confirmation = Confirmation::WhenRisky;
        assert_eq!(engine.evaluate(&s, &CallFacts::default(), Origin::Agent), Decision::Allow);
        let facts = CallFacts { overwrites: true, ..Default::default() };
        assert!(matches!(engine.evaluate(&s, &facts, Origin::Agent), Decision::RequireConfirmation { .. }));
    }
}
