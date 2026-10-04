//! Zusammenbau von JARVIS aus der Konfiguration. Wird vom CLI und später
//! von der Tauri-App genutzt.

use jarvis_integrations::http::{ReadOnlyApi, TokenProvider, WebClient};
use jarvis_integrations::oauth::MsDeviceCodeFlow;
use jarvis_integrations::web::search::{Brave, DuckDuckGo, SearchProvider, Searxng};
use jarvis_integrations::{fs, mail, teams, web, webuntis};
use jarvis_memory::{Memory, SqliteAudit};
use jarvis_models::{ModelManager, ModelSettings};
use jarvis_permissions::PolicyEngine;
use jarvis_resources::{detect_hardware, recommend_profile, Hardware, ModelProfile};
use jarvis_runtime::{Confirmer, ServiceManager, Tool, ToolGateway, ToolRegistry};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub filesystem: FsConfig,
    pub web: WebConfig,
    pub models: ModelsConfig,
    pub mail: MailConfig,
    pub microsoft: MicrosoftConfig,
    pub webuntis: UntisConfig,
    pub voice: VoiceConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FsConfig {
    pub roots: Vec<String>,
}
impl Default for FsConfig {
    fn default() -> Self {
        Self { roots: vec!["~/Documents".into(), "~/Desktop".into(), "~/Downloads".into()] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    pub searxng_url: String,
    pub duckduckgo_fallback: bool,
}
impl Default for WebConfig {
    fn default() -> Self {
        Self { searxng_url: String::new(), duckduckgo_fallback: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelsConfig {
    /// Leer = automatisch passend zur Hardware.
    pub main: String,
    pub fallback: String,
    pub embedding: String,
    pub ollama_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MailConfig {
    /// "outlook", "gmail" oder leer (aus).
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MicrosoftConfig {
    /// Client-ID einer eigenen (kostenlosen) Entra-App-Registrierung.
    pub client_id: String,
    pub tenant: String,
    pub teams_enabled: bool,
}
impl Default for MicrosoftConfig {
    fn default() -> Self {
        Self { client_id: String::new(), tenant: "organizations".into(), teams_enabled: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UntisConfig {
    pub server: String,
    pub school: String,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    pub whisper_binary: String,
    pub whisper_model_dir: String,
    pub tts: String,
    pub say_voice: String,
    pub piper_model: String,
}
impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            whisper_binary: "whisper-cli".into(),
            whisper_model_dir: "~/Library/Application Support/JARVIS/models/whisper".into(),
            tts: "say".into(),
            say_voice: "Anna".into(),
            piper_model: String::new(),
        }
    }
}

pub fn expand(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(p),
    }
}

/// JARVIS-Datenordner (Memory, Audit, Konfiguration). Für den Agenten gesperrt.
pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("JARVIS_DATA_DIR") {
        return PathBuf::from(d);
    }
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("JARVIS")
}

pub fn load_config(dir: &Path) -> Result<Config, String> {
    let p = dir.join("config.toml");
    if !p.exists() {
        return Ok(Config::default());
    }
    toml::from_str(&std::fs::read_to_string(&p).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", p.display()))
}

/// Geheimnis aus Umgebungsvariable `JARVIS_<NAME>` oder dem macOS-Schlüsselbund
/// (Dienst "JARVIS", Konto `<name>`). Niemals aus der Konfigurationsdatei.
pub fn secret(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(format!("JARVIS_{}", name.to_uppercase())) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("security").args(["find-generic-password", "-s", "JARVIS", "-a", name, "-w"]).output().ok()?;
        if out.status.success() {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return (!v.is_empty()).then_some(v);
        }
    }
    None
}

pub fn store_secret(name: &str, value: &str) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("Schlüsselbund nur unter macOS verfügbar; bitte JARVIS_<NAME> setzen".into());
    }
    let st = std::process::Command::new("security")
        .args(["add-generic-password", "-U", "-s", "JARVIS", "-a", name, "-w", value])
        .status()
        .map_err(|e| e.to_string())?;
    st.success().then_some(()).ok_or_else(|| "Schlüsselbund-Eintrag fehlgeschlagen".into())
}

pub fn model_profile(cfg: &Config, hw: &Hardware) -> ModelProfile {
    let mut p = recommend_profile(hw);
    if !cfg.models.main.is_empty() {
        p.main = cfg.models.main.clone();
    }
    if !cfg.models.fallback.is_empty() {
        p.fallback = cfg.models.fallback.clone();
    }
    if !cfg.models.embedding.is_empty() {
        p.embedding = cfg.models.embedding.clone();
    }
    p
}

pub struct App {
    pub config: Config,
    pub hardware: Hardware,
    pub memory: Memory,
    pub audit: SqliteAudit,
    pub models: Arc<ModelManager>,
    pub research: Arc<web::Research>,
    pub gateway: ToolGateway,
    /// Integrationen, die mangels Konfiguration nicht aktiv sind (mit Grund).
    pub inactive: Vec<(String, String)>,
}

pub fn search_providers(cfg: &Config, client: &WebClient) -> Vec<Arc<dyn SearchProvider>> {
    let mut v: Vec<Arc<dyn SearchProvider>> = vec![];
    if !cfg.web.searxng_url.is_empty() {
        v.push(Arc::new(Searxng { client: WebClient::new_allowing_private(), base_url: cfg.web.searxng_url.clone() }));
    }
    if let Some(k) = secret("brave_api_key") {
        v.push(Arc::new(Brave::new(client.clone(), k)));
    }
    if cfg.web.duckduckgo_fallback {
        v.push(Arc::new(DuckDuckGo::new(client.clone())));
    }
    v
}

pub fn microsoft_tokens(cfg: &Config) -> Option<Arc<dyn TokenProvider>> {
    if cfg.microsoft.client_id.is_empty() {
        return None;
    }
    let refresh = secret("ms_refresh_token")?;
    Some(Arc::new(MsDeviceCodeFlow::new(cfg.microsoft.client_id.clone(), &cfg.microsoft.tenant).with_refresh_token(refresh)))
}

/// Baut die komplette Laufzeit. `confirmer` ist der UI-Dialog.
pub fn build(dir: &Path, confirmer: Arc<dyn Confirmer>) -> Result<App, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let config = load_config(dir)?;
    let hardware = detect_hardware();
    let memory = Memory::open(&dir.join("brain.db")).map_err(|e| e.to_string())?;
    let audit = SqliteAudit::new(memory.clone());

    let mut settings = ModelSettings::new(model_profile(&config, &hardware));
    if !config.models.ollama_url.is_empty() {
        settings.base_url = config.models.ollama_url.clone();
    }
    let models = Arc::new(ModelManager::new(settings));
    let services = ServiceManager::new();
    services.register(models.clone());

    let client = WebClient::new();
    let research = Arc::new(web::Research::new(client.clone(), search_providers(&config, &client), memory.clone()));

    let protected = vec![dir.to_path_buf()];
    let sandbox = Arc::new(fs::FsSandbox::new(config.filesystem.roots.iter().map(|r| expand(r)).filter(|p| p.exists()).collect(), protected.clone()));

    let mut tools: Vec<Arc<dyn Tool>> = vec![];
    let mut inactive = vec![];
    tools.extend(fs::tools(sandbox, Arc::new(fs::SystemOpener)));
    tools.extend(web::tools(research.clone()));

    let ms = microsoft_tokens(&config);
    match (config.mail.provider.as_str(), &ms) {
        ("outlook", Some(t)) => tools.extend(mail::tools(Arc::new(mail::GraphMail {
            api: ReadOnlyApi::new("https://graph.microsoft.com/v1.0", t.clone(), &[]),
        }))),
        ("gmail", _) => match secret("gmail_access_token") {
            Some(tok) => tools.extend(mail::tools(Arc::new(mail::Gmail {
                api: ReadOnlyApi::new("https://gmail.googleapis.com", Arc::new(jarvis_integrations::http::StaticToken(tok)), &[]),
            }))),
            None => inactive.push(("email".into(), "Gmail-Token fehlt (Anmeldung folgt in der App)".into())),
        },
        ("outlook", None) => inactive.push(("email".into(), "Microsoft nicht angemeldet: `jarvis login microsoft`".into())),
        _ => inactive.push(("email".into(), "mail.provider nicht gesetzt".into())),
    }
    match (&ms, config.microsoft.teams_enabled) {
        (Some(t), true) => tools.extend(teams::tools(Arc::new(teams::TeamsReader {
            api: ReadOnlyApi::new("https://graph.microsoft.com/v1.0", t.clone(), teams::GRAPH_ALLOWED_POST),
        }))),
        (None, true) => inactive.push(("teams".into(), "Microsoft nicht angemeldet: `jarvis login microsoft`".into())),
        _ => inactive.push(("teams".into(), "microsoft.teams_enabled = false".into())),
    }
    match secret("webuntis_password") {
        Some(pw) if !config.webuntis.server.is_empty() => tools.extend(webuntis::tools(Arc::new(webuntis::UntisClient::new(webuntis::UntisConfig {
            server: config.webuntis.server.clone(),
            school: config.webuntis.school.clone(),
            username: config.webuntis.username.clone(),
            password: pw,
        })))),
        _ => inactive.push(("webuntis".into(), "webuntis.server/school/username oder Passwort im Schlüsselbund fehlt".into())),
    }

    let mut registry = ToolRegistry::new();
    for t in tools {
        registry.register(t).map_err(|e| e.to_string())?;
    }
    let gateway = ToolGateway::new(registry, PolicyEngine::new(protected), services, confirmer, Arc::new(audit.clone()));
    Ok(App { config, hardware, memory, audit, models, research, gateway, inactive })
}

pub const EXAMPLE_CONFIG: &str = r#"# JARVIS-Konfiguration (keine Passwörter oder Tokens hier eintragen!)
# Geheimnisse liegen im macOS-Schlüsselbund (Dienst "JARVIS").

[filesystem]
roots = ["~/Documents", "~/Desktop", "~/Downloads"]

[web]
searxng_url = ""            # optional, z. B. "http://127.0.0.1:8888"
duckduckgo_fallback = true  # Suche ohne API-Key

[models]
main = ""        # leer = automatisch (16 GB → qwen3:8b)
fallback = ""    # leer = automatisch (16 GB → qwen3:4b)
embedding = ""   # leer = nomic-embed-text

[mail]
provider = ""    # "outlook" oder "gmail"

[microsoft]
client_id = ""   # eigene Entra-App (öffentlicher Client, nur Lese-Berechtigungen)
tenant = "organizations"
teams_enabled = false

[webuntis]
server = ""      # z. B. "https://xyz.webuntis.com"
school = ""
username = ""

[voice]
whisper_binary = "whisper-cli"
whisper_model_dir = "~/Library/Application Support/JARVIS/models/whisper"
tts = "say"      # "say" oder "piper"
say_voice = "Anna"
piper_model = ""
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_runtime::gateway::DenyAllConfirmer;

    #[test]
    fn example_config_parses_and_builds() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), EXAMPLE_CONFIG).unwrap();
        let c = load_config(d.path()).unwrap();
        assert_eq!(c.voice.say_voice, "Anna");
        let app = build(d.path(), Arc::new(DenyAllConfirmer)).unwrap();
        let names = app.gateway.registry().names();
        assert!(names.contains(&"fs_read") && names.contains(&"web_research"));
        // Ohne Konfiguration keine Mail/Teams/WebUntis-Tools.
        assert!(!names.iter().any(|n| n.starts_with("email_") || n.starts_with("teams_") || n.starts_with("webuntis_")));
        assert_eq!(app.inactive.len(), 3);
    }
}
