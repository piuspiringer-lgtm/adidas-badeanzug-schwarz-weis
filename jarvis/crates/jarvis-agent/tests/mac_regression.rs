//! Regression des echten Mac-Tests
//! `jarvis agent -v "Finde meine PDFs im Ordner Dokumente"`:
//! 6 Tools angeboten, englischer Denktext in der Antwort, sehr lange Laufzeit.
//! Läuft über den echten ModelManager gegen einen Ollama-Mock.

use jarvis_agent::{Agent, AgentConfig, AgentEvent, FixedMode};
use jarvis_integrations::fs::{self, FsSandbox, SystemOpener};
use jarvis_memory::{Memory, SqliteAudit};
use jarvis_models::{ModelManager, ModelSettings, MAX_OUTPUT_TOKENS};
use jarvis_permissions::PolicyEngine;
use jarvis_resources::{Mode, ModelProfile};
use jarvis_runtime::gateway::DenyAllConfirmer;
use jarvis_runtime::{ServiceManager, ToolGateway, ToolRegistry};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LEAK: &str = "Okay, let's see. The user asked to find PDFs in the Documents folder. \
I used fs_search and got two results, so I will answer in German and list them.\n</think>\n\n";

#[tokio::test]
async fn pdf_search_uses_one_tool_and_never_leaks_reasoning() {
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().canonicalize().unwrap().join("Documents");
    std::fs::create_dir_all(docs.join("Schule")).unwrap();
    std::fs::write(docs.join("Rechnung.pdf"), "x").unwrap();
    std::fs::write(docs.join("Schule/Zeugnis.pdf"), "x").unwrap();
    std::fs::write(docs.join("notiz.txt"), "x").unwrap();

    let ollama = MockServer::start().await;
    // 2. Aufruf (nach dem Tool-Ergebnis): Denktext + echte Antwort.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "qwen3:8b", "done": true, "prompt_eval_count": 600, "eval_count": 60,
            "message": {"role": "assistant", "content": format!("{LEAK}Ich habe 2 PDFs gefunden: Rechnung.pdf und Schule/Zeugnis.pdf.")}
        })))
        .with_priority(1)
        .mount(&ollama)
        .await;
    // 1. Aufruf: Tool-Call fs_search (ebenfalls mit Denktext im Inhalt).
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "qwen3:8b", "done": true, "prompt_eval_count": 500, "eval_count": 40,
            "message": {"role": "assistant", "content": LEAK,
                "tool_calls": [{"function": {"name": "fs_search", "arguments": {"root": docs, "pattern": "*.pdf"}}}]}
        })))
        .with_priority(2)
        .mount(&ollama)
        .await;

    let profile = ModelProfile {
        main: "qwen3:8b".into(),
        fallback: "qwen3:4b".into(),
        embedding: "nomic-embed-text".into(),
        context_window: 8192,
        stt_model: "ggml-small.bin".into(),
        stt_fallback: "ggml-small.bin".into(),
        ai_ram_budget_gb: 7.0,
    };
    let mut settings = ModelSettings::new(profile);
    settings.base_url = ollama.uri();
    settings.auto_start_server = false;
    let models = Arc::new(ModelManager::new(settings));

    let memory = Memory::in_memory().unwrap();
    let mut reg = ToolRegistry::new();
    for t in fs::tools(Arc::new(FsSandbox::new(vec![docs.clone()], vec![])), Arc::new(SystemOpener))
        .into_iter()
        .chain(jarvis_integrations::memory::tools(memory.clone()))
    {
        reg.register(t).unwrap();
    }
    let gw = Arc::new(ToolGateway::new(reg, PolicyEngine::new(vec![]), ServiceManager::new(), Arc::new(DenyAllConfirmer), Arc::new(SqliteAudit::new(memory.clone()))));
    let agent = Agent::new(gw, models, memory, Arc::new(FixedMode(Mode::Performance)), AgentConfig { fs_roots: vec![docs.clone()], ..Default::default() });

    let events = Arc::new(Mutex::new(vec![]));
    let ev2 = events.clone();
    let out = agent.run("Finde meine PDFs im Ordner Dokumente", Arc::new(move |e| ev2.lock().unwrap().push(e))).await;

    // 3. Modellausgabe: nur die Antwort für den Benutzer.
    assert_eq!(out.answer, "Ich habe 2 PDFs gefunden: Rechnung.pdf und Schule/Zeugnis.pdf.");
    for leak in ["let's see", "The user", "</think>", "<think>"] {
        assert!(!out.answer.contains(leak), "Denktext in Antwort: {leak}");
    }
    // 2. Tool-Routing: genau ein Tool angeboten und ausgeführt.
    let reqs = ollama.received_requests().await.unwrap();
    let chats: Vec<Value> = reqs.iter().filter(|r| r.url.path() == "/api/chat").map(|r| serde_json::from_slice(&r.body).unwrap()).collect();
    assert_eq!(chats.len(), 2, "genau 2 Modellaufrufe");
    for c in &chats {
        let offered: Vec<&str> = c["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap()).collect();
        assert_eq!(offered, vec!["fs_search"]);
        // 1. Laufzeit: kein Denkmodus, begrenzte Länge, Hauptmodell.
        assert_eq!(c["think"], json!(false));
        assert_eq!(c["options"]["num_predict"], json!(MAX_OUTPUT_TOKENS));
        assert_eq!(c["model"], json!("qwen3:8b"));
        assert!(c["messages"][0]["content"].as_str().unwrap().ends_with("/no_think"));
    }
    // Der Denktext aus Aufruf 1 wird auch nicht in den Verlauf zurückgespielt.
    let second = &chats[1]["messages"];
    assert!(second.as_array().unwrap().iter().all(|m| !m["content"].as_str().unwrap_or("").contains("let's see")));
    assert_eq!(out.steps.len(), 1);
    assert_eq!(out.steps[0].tool, "fs_search");
    assert!(out.steps[0].summary.starts_with("2 Treffer"), "{}", out.steps[0].summary);
    // Messung vorhanden, Denktext gezählt (nicht gezeigt).
    assert_eq!(out.llm_calls, 2);
    let calls: Vec<usize> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            AgentEvent::LlmCall { hidden_reasoning_chars, .. } => Some(*hidden_reasoning_chars),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| *c > 100));
}
