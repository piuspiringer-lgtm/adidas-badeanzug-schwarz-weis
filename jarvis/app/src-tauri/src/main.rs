//! JARVIS Desktop (Tauri). Dünne Schicht über `jarvis_app::App` und dem
//! Agent Core: Befehle für die Oberfläche, Live-Events des Agenten und ein
//! nativer Bestätigungsdialog als [`Confirmer`].
//!
//! Sicherheit: Die Oberfläche zeigt Modell- und Tool-Texte nur als Text an
//! (kein HTML-Rendering), die CSP erlaubt keine fremden Skripte. Eine
//! Bestätigung kann nur für eine gerade offene Anfrage (zufällige ID)
//! abgegeben werden und verfällt nach 2 Minuten als "Nein".

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use async_trait::async_trait;
use jarvis_agent::{Agent, AgentEvent, Outcome};
use jarvis_app::App;
use jarvis_permissions::{CallFacts, ToolSpec};
use jarvis_runtime::Confirmer;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;

const CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Serialize, Clone)]
struct ConfirmRequest {
    id: String,
    tool: String,
    description: String,
    access: String,
    risk: String,
    reason: String,
    paths: Vec<String>,
}

/// Bestätigung über einen Dialog in der Oberfläche.
#[derive(Default)]
struct UiConfirmer {
    handle: OnceLock<AppHandle>,
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

fn random_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    let a = h.finish();
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(a);
    format!("{a:016x}{:016x}", h2.finish())
}

#[async_trait]
impl Confirmer for UiConfirmer {
    async fn confirm(&self, spec: &ToolSpec, facts: &CallFacts, reason: &str) -> bool {
        let Some(app) = self.handle.get() else { return false };
        let id = random_id();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let req = ConfirmRequest {
            id: id.clone(),
            tool: spec.name.into(),
            description: spec.description.into(),
            access: format!("{:?}", spec.access),
            risk: format!("{:?}", spec.risk),
            reason: reason.into(),
            paths: facts.paths.iter().map(|p| p.display().to_string()).collect(),
        };
        if app.emit("confirm-request", &req).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return false;
        }
        let approved = matches!(tokio::time::timeout(CONFIRM_TIMEOUT, rx).await, Ok(Ok(true)));
        self.pending.lock().unwrap().remove(&id);
        let _ = app.emit("confirm-closed", &id);
        approved
    }
}

struct Core {
    app: App,
    agent: Arc<Agent>,
    confirmer: Arc<UiConfirmer>,
    busy: tokio::sync::Mutex<()>,
    monitor: Mutex<jarvis_resources::Monitor>,
}

#[derive(Serialize)]
struct Status {
    hardware: jarvis_resources::Hardware,
    snapshot: jarvis_resources::Snapshot,
    mode: jarvis_resources::Mode,
    profile: jarvis_resources::ModelProfile,
    services: Vec<jarvis_runtime::lifecycle::ServiceStatus>,
    inactive: Vec<(String, String)>,
    fs_roots: Vec<String>,
    busy: bool,
}

#[tauri::command]
async fn send_message(text: String, core: State<'_, Core>, app: AppHandle) -> Result<Outcome, String> {
    let text = text.trim().to_string();
    if text.is_empty() || text.chars().count() > 4000 {
        return Err("Nachricht leer oder zu lang".into());
    }
    let _guard = core.busy.try_lock().map_err(|_| "JARVIS arbeitet noch an der vorherigen Anfrage".to_string())?;
    let sink: jarvis_agent::EventSink = Arc::new(move |e: AgentEvent| {
        let _ = app.emit("agent-event", &e);
    });
    Ok(core.agent.run(&text, sink).await)
}

#[tauri::command]
fn confirm_response(id: String, approved: bool, core: State<'_, Core>) -> Result<(), String> {
    match core.confirmer.pending.lock().unwrap().remove(&id) {
        Some(tx) => {
            let _ = tx.send(approved);
            Ok(())
        }
        None => Err("Keine offene Bestätigung mit dieser ID".into()),
    }
}

#[tauri::command]
async fn reset_conversation(core: State<'_, Core>) -> Result<(), String> {
    core.agent.reset().await;
    Ok(())
}

#[tauri::command]
fn system_status(core: State<'_, Core>) -> Status {
    let snapshot = core.monitor.lock().unwrap().snapshot();
    Status {
        hardware: core.app.hardware.clone(),
        mode: jarvis_resources::decide_mode(&snapshot),
        snapshot,
        profile: core.app.models.profile(),
        services: core.app.gateway.services().status(),
        inactive: core.app.inactive.clone(),
        fs_roots: core.app.fs_roots.iter().map(|p| p.display().to_string()).collect(),
        busy: core.busy.try_lock().is_err(),
    }
}

#[tauri::command]
fn list_tools(core: State<'_, Core>) -> Vec<jarvis_runtime::tool::ToolInfo> {
    core.app.gateway.registry().list()
}

#[derive(Serialize)]
struct AuditView {
    entries: Vec<jarvis_memory::AuditEntry>,
    chain_ok: bool,
    checked: usize,
}

#[tauri::command]
fn audit_log(limit: usize, core: State<'_, Core>) -> Result<AuditView, String> {
    let entries = core.app.audit.recent(limit.min(500)).map_err(|e| e.to_string())?;
    let v = core.app.audit.verify().map_err(|e| e.to_string())?;
    Ok(AuditView { entries, chain_ok: v.is_ok(), checked: v.unwrap_or(0) })
}

#[derive(Serialize)]
struct MemoryView {
    preferences: Vec<(String, String)>,
}

#[tauri::command]
fn memory_overview(core: State<'_, Core>) -> Result<MemoryView, String> {
    Ok(MemoryView { preferences: core.app.memory.preferences(50).map_err(|e| e.to_string())? })
}

fn main() {
    tauri::Builder::default()
        .setup(|tauri_app| {
            let confirmer = Arc::new(UiConfirmer::default());
            let _ = confirmer.handle.set(tauri_app.handle().clone());
            let dir = jarvis_app::data_dir();
            let app = jarvis_app::build(&dir, confirmer.clone())?;
            // Reaper und Agent im Tokio-Kontext von Tauri anlegen.
            let agent = Arc::new(tauri::async_runtime::block_on(async { app.agent() }));
            tauri_app.manage(Core {
                app,
                agent,
                confirmer,
                busy: tokio::sync::Mutex::new(()),
                monitor: Mutex::new(jarvis_resources::Monitor::new()),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Beim Schließen alle Dienste (Ollama, Modelle) sauber entladen.
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if let Some(core) = window.try_state::<Core>() {
                    let services = core.app.gateway.services().clone();
                    tauri::async_runtime::block_on(async move {
                        services.unload_all_unused().await;
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![send_message, confirm_response, reset_conversation, system_status, list_tools, audit_log, memory_overview])
        .run(tauri::generate_context!())
        .expect("JARVIS konnte nicht gestartet werden");
}
