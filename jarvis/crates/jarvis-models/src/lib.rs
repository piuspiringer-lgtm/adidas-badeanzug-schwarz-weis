//! Model Manager: lokales LLM über Ollama, mit Fallback-Modell und der
//! Garantie, dass immer höchstens ein Chat-Modell geladen ist.

use async_trait::async_trait;
use jarvis_context::{Message, ModelTier, Summarizer};
use jarvis_resources::ModelProfile;
use jarvis_runtime::Service;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("Ollama nicht erreichbar: {0}")]
    Unreachable(String),
    #[error("Modell '{0}' nicht installiert")]
    NotInstalled(String),
    #[error("Ollama-Fehler ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("ungültige Antwort: {0}")]
    Decode(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModel {
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedModel {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub size_vram: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatResult {
    pub model: String,
    pub content: String,
    pub tool_calls: Vec<Value>,
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    pub used_fallback: bool,
}

/// Dünner HTTP-Client für die Ollama-API (nur localhost).
#[derive(Clone)]
pub struct OllamaClient {
    base: String,
    http: reqwest::Client,
}

impl OllamaClient {
    pub fn new(base: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("HTTP-Client");
        Self { base: base.into().trim_end_matches('/').to_string(), http }
    }

    async fn get(&self, path: &str) -> Result<Value, ModelError> {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .map_err(|e| ModelError::Unreachable(e.to_string()))?;
        Self::decode(r).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, ModelError> {
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .map_err(|e| ModelError::Unreachable(e.to_string()))?;
        Self::decode(r).await
    }

    async fn decode(r: reqwest::Response) -> Result<Value, ModelError> {
        let status = r.status().as_u16();
        let text = r.text().await.map_err(|e| ModelError::Decode(e.to_string()))?;
        if status >= 400 {
            return Err(ModelError::Api { status, body: text });
        }
        serde_json::from_str(&text).map_err(|e| ModelError::Decode(e.to_string()))
    }

    pub async fn version(&self) -> Result<String, ModelError> {
        Ok(self.get("/api/version").await?["version"].as_str().unwrap_or("?").to_string())
    }

    pub async fn installed(&self) -> Result<Vec<InstalledModel>, ModelError> {
        serde_json::from_value(self.get("/api/tags").await?["models"].clone()).map_err(|e| ModelError::Decode(e.to_string()))
    }

    pub async fn loaded(&self) -> Result<Vec<LoadedModel>, ModelError> {
        serde_json::from_value(self.get("/api/ps").await?["models"].clone()).map_err(|e| ModelError::Decode(e.to_string()))
    }

    pub async fn unload(&self, model: &str) -> Result<(), ModelError> {
        self.post("/api/generate", &json!({"model": model, "keep_alive": 0})).await.map(|_| ())
    }

    pub async fn chat(&self, model: &str, messages: &[Message], tools: &[Value], num_ctx: usize, keep_alive: u64, think: bool) -> Result<ChatResult, ModelError> {
        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "keep_alive": format!("{keep_alive}s"),
            "think": think,
            "options": {"num_ctx": num_ctx, "temperature": 0.3}
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
        }
        let v = self.post("/api/chat", &body).await.map_err(|e| match e {
            ModelError::Api { status: 404, .. } => ModelError::NotInstalled(model.into()),
            e => e,
        })?;
        Ok(ChatResult {
            model: model.into(),
            content: v["message"]["content"].as_str().unwrap_or_default().to_string(),
            tool_calls: v["message"]["tool_calls"].as_array().cloned().unwrap_or_default(),
            prompt_tokens: v["prompt_eval_count"].as_u64().unwrap_or(0),
            output_tokens: v["eval_count"].as_u64().unwrap_or(0),
            used_fallback: false,
        })
    }

    pub async fn embed(&self, model: &str, input: &[String], keep_alive: u64) -> Result<Vec<Vec<f32>>, ModelError> {
        let v = self.post("/api/embed", &json!({"model": model, "input": input, "keep_alive": format!("{keep_alive}s")})).await?;
        serde_json::from_value(v["embeddings"].clone()).map_err(|e| ModelError::Decode(e.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct ModelSettings {
    pub base_url: String,
    pub profile: ModelProfile,
    pub keep_alive_secs: u64,
    /// `ollama serve` bei Bedarf selbst starten (und beim Entladen beenden).
    pub auto_start_server: bool,
    pub ollama_binary: String,
}

impl ModelSettings {
    pub fn new(profile: ModelProfile) -> Self {
        Self {
            base_url: "http://127.0.0.1:11434".into(),
            profile,
            keep_alive_secs: 300,
            auto_start_server: true,
            ollama_binary: "ollama".into(),
        }
    }
}

pub struct ModelManager {
    client: OllamaClient,
    settings: Mutex<ModelSettings>,
    current: Mutex<Option<String>>,
    spawned: Mutex<Option<tokio::process::Child>>,
    force_fallback: Mutex<bool>,
}

impl ModelManager {
    pub fn new(settings: ModelSettings) -> Self {
        Self {
            client: OllamaClient::new(settings.base_url.clone()),
            settings: Mutex::new(settings),
            current: Mutex::new(None),
            spawned: Mutex::new(None),
            force_fallback: Mutex::new(false),
        }
    }

    pub fn client(&self) -> &OllamaClient {
        &self.client
    }

    pub fn profile(&self) -> ModelProfile {
        self.settings.lock().unwrap().profile.clone()
    }

    /// Modelle zur Laufzeit wechseln (z. B. aus den Einstellungen).
    pub fn set_profile(&self, p: ModelProfile) {
        self.settings.lock().unwrap().profile = p;
    }

    /// Vom Resource Manager gesetzt (Akku/Thermik/Speicher).
    pub fn set_force_fallback(&self, v: bool, keep_alive_secs: u64) {
        *self.force_fallback.lock().unwrap() = v;
        self.settings.lock().unwrap().keep_alive_secs = keep_alive_secs;
    }

    pub fn model_for(&self, tier: ModelTier) -> String {
        let s = self.settings.lock().unwrap();
        if *self.force_fallback.lock().unwrap() {
            return s.profile.fallback.clone();
        }
        match tier {
            ModelTier::LocalSmall => s.profile.fallback.clone(),
            // "Strong" ist ohne explizite Konfiguration das lokale Hauptmodell.
            ModelTier::LocalMain | ModelTier::Strong => s.profile.main.clone(),
        }
    }

    /// Stellt sicher, dass nur `model` geladen ist (entlädt ein anderes zuvor).
    async fn switch_to(&self, model: &str) {
        let prev = self.current.lock().unwrap().clone();
        if let Some(p) = prev {
            if p != model {
                let _ = self.client.unload(&p).await;
            }
        }
        *self.current.lock().unwrap() = Some(model.to_string());
    }

    pub async fn chat(&self, tier: ModelTier, messages: &[Message], tools: &[Value]) -> Result<ChatResult, ModelError> {
        let (ctx, keep, fallback) = {
            let s = self.settings.lock().unwrap();
            (s.profile.context_window, s.keep_alive_secs, s.profile.fallback.clone())
        };
        let model = self.model_for(tier);
        let think = tier == ModelTier::Strong;
        self.switch_to(&model).await;
        match self.client.chat(&model, messages, tools, ctx, keep, think).await {
            Ok(r) => Ok(r),
            Err(e @ ModelError::Unreachable(_)) => Err(e),
            Err(_) if model != fallback => {
                self.switch_to(&fallback).await;
                let mut r = self.client.chat(&fallback, messages, tools, ctx, keep, false).await?;
                r.used_fallback = true;
                Ok(r)
            }
            Err(e) => Err(e),
        }
    }

    pub async fn embed(&self, input: &[String]) -> Result<Vec<Vec<f32>>, ModelError> {
        let m = self.settings.lock().unwrap().profile.embedding.clone();
        self.client.embed(&m, input, 60).await
    }

    /// Prüft, welche Profil-Modelle installiert sind.
    pub async fn missing_models(&self) -> Result<Vec<String>, ModelError> {
        let installed: Vec<String> = self.client.installed().await?.into_iter().map(|m| m.name).collect();
        let p = self.profile();
        Ok([p.main, p.fallback, p.embedding]
            .into_iter()
            .filter(|w| !installed.iter().any(|i| i == w || i.strip_suffix(":latest") == Some(w)))
            .collect())
    }
}

#[async_trait]
impl Service for ModelManager {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.settings.lock().unwrap().keep_alive_secs.max(30))
    }

    async fn load(&self) -> Result<(), String> {
        if self.client.version().await.is_ok() {
            return Ok(());
        }
        let (auto, bin) = {
            let s = self.settings.lock().unwrap();
            (s.auto_start_server, s.ollama_binary.clone())
        };
        if !auto {
            return Err("Ollama läuft nicht (Autostart deaktiviert)".into());
        }
        let child = tokio::process::Command::new(&bin)
            .arg("serve")
            .env("OLLAMA_MAX_LOADED_MODELS", "1")
            .env("OLLAMA_NUM_PARALLEL", "1")
            .env("OLLAMA_FLASH_ATTENTION", "1")
            .env("OLLAMA_KV_CACHE_TYPE", "q8_0")
            .env("OLLAMA_HOST", "127.0.0.1:11434")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("'{bin} serve' konnte nicht gestartet werden: {e}"))?;
        *self.spawned.lock().unwrap() = Some(child);
        for _ in 0..50 {
            if self.client.version().await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err("Ollama hat nicht rechtzeitig geantwortet".into())
    }

    async fn unload(&self) -> Result<(), String> {
        let current = self.current.lock().unwrap().take();
        if let Some(m) = current {
            let _ = self.client.unload(&m).await;
        }
        let child = self.spawned.lock().unwrap().take();
        if let Some(mut c) = child {
            let _ = c.kill().await;
        }
        Ok(())
    }
}

/// Zusammenfassungen laufen immer auf dem kleinen lokalen Modell.
#[async_trait]
impl Summarizer for ModelManager {
    async fn summarize(&self, text: &str, max_tokens: usize) -> Result<String, String> {
        let msgs = [
            Message::new(
                "system",
                format!("Fasse den Text sachlich auf Deutsch in höchstens {max_tokens} Tokens zusammen. Behalte Zahlen, Daten, Namen und Quellenangaben."),
            ),
            Message::new("user", text),
        ];
        self.chat(ModelTier::LocalSmall, &msgs, &[]).await.map(|r| r.content).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn profile() -> ModelProfile {
        ModelProfile {
            main: "qwen3:8b".into(),
            fallback: "qwen3:4b".into(),
            embedding: "nomic-embed-text".into(),
            context_window: 8192,
            stt_model: "x".into(),
            stt_fallback: "y".into(),
            ai_ram_budget_gb: 7.0,
        }
    }

    fn manager(url: &str) -> ModelManager {
        let mut s = ModelSettings::new(profile());
        s.base_url = url.into();
        s.auto_start_server = false;
        ModelManager::new(s)
    }

    fn chat_ok(model: &str, text: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "model": model, "message": {"role": "assistant", "content": text}, "done": true,
            "prompt_eval_count": 12, "eval_count": 5
        }))
    }

    #[tokio::test]
    async fn chat_uses_main_model_with_context_settings() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_partial_json(json!({"model": "qwen3:8b", "stream": false, "options": {"num_ctx": 8192}})))
            .respond_with(chat_ok("qwen3:8b", "Hallo!"))
            .expect(1)
            .mount(&s)
            .await;
        let m = manager(&s.uri());
        let r = m.chat(ModelTier::LocalMain, &[Message::new("user", "Hi")], &[]).await.unwrap();
        assert_eq!(r.content, "Hallo!");
        assert_eq!(r.prompt_tokens, 12);
        assert!(!r.used_fallback);
    }

    #[tokio::test]
    async fn falls_back_when_main_missing_and_unloads_previous() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_partial_json(json!({"model": "qwen3:8b"})))
            .respond_with(ResponseTemplate::new(404).set_body_string("model not found"))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_partial_json(json!({"model": "qwen3:4b"})))
            .respond_with(chat_ok("qwen3:4b", "Fallback"))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .and(body_partial_json(json!({"model": "qwen3:8b", "keep_alive": 0})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"done": true})))
            .expect(1)
            .mount(&s)
            .await;
        let m = manager(&s.uri());
        let r = m.chat(ModelTier::LocalMain, &[Message::new("user", "Hi")], &[]).await.unwrap();
        assert!(r.used_fallback);
        assert_eq!(r.model, "qwen3:4b");
    }

    #[tokio::test]
    async fn force_fallback_and_missing_models() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [{"name": "qwen3:4b", "size": 1}, {"name": "nomic-embed-text:latest", "size": 1}]})))
            .mount(&s)
            .await;
        let m = manager(&s.uri());
        assert_eq!(m.missing_models().await.unwrap(), vec!["qwen3:8b".to_string()]);
        m.set_force_fallback(true, 30);
        assert_eq!(m.model_for(ModelTier::LocalMain), "qwen3:4b");
    }

    #[tokio::test]
    async fn unreachable_server_reports_error_without_autostart() {
        let m = manager("http://127.0.0.1:9");
        assert!(matches!(m.chat(ModelTier::LocalMain, &[], &[]).await, Err(ModelError::Unreachable(_))));
        assert!(m.load().await.is_err());
    }

    /// Echter Test gegen ein lokales Ollama: `cargo test -p jarvis-models -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn live_ollama() {
        let url = std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
        let mut p = profile();
        if let Ok(m) = std::env::var("JARVIS_TEST_MODEL") {
            p.main = m.clone();
            p.fallback = m;
        }
        let mut s = ModelSettings::new(p);
        s.base_url = url;
        let m = ModelManager::new(s);
        m.load().await.expect("Ollama starten/erreichen");
        let r = m.chat(ModelTier::LocalSmall, &[Message::new("user", "Antworte nur mit: OK")], &[]).await.unwrap();
        println!("Antwort von {}: {}", r.model, r.content);
        assert!(!r.content.is_empty());
        m.unload().await.unwrap();
    }
}
