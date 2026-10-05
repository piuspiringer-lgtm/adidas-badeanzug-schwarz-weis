//! `jarvis` – Kommandozeile zum Einrichten und Testen der Infrastruktur.

use async_trait::async_trait;
use clap::{Parser, Subcommand};
use jarvis_app::*;
use jarvis_context::{Message, Router};
use jarvis_permissions::{CallFacts, Origin, ToolSpec};
use jarvis_resources::{decide_mode, settings_for, Monitor};
use jarvis_runtime::{Confirmer, Service};
use jarvis_voice::{MacSay, SpeechToText, TextToSpeech, WhisperCli};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "jarvis", version, about = "JARVIS – lokaler Assistent (Infrastruktur-Werkzeug)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Beispiel-Konfiguration anlegen (überschreibt nichts).
    Init,
    /// Prüft alle Komponenten. Mit --online auch Netz-Integrationen (nur lesend).
    Doctor {
        #[arg(long)]
        online: bool,
    },
    /// Listet alle Tools mit Berechtigungen.
    Tools,
    /// Führt ein Tool über den Gateway aus, z. B. `jarvis run fs_list '{"path":"~/Documents"}'`.
    Run { tool: String, #[arg(default_value = "{}")] args: String },
    /// Führt einen Auftrag mit dem Agenten aus (Tools, Bestätigungen, Memory).
    Agent {
        task: Vec<String>,
        /// Alle Agent-Schritte ausführlich anzeigen.
        #[arg(long, short)]
        verbose: bool,
    },
    /// Interaktiver Dialog mit JARVIS (Agent mit Gesprächsverlauf).
    Chat {
        #[arg(long, short)]
        verbose: bool,
    },
    /// Fragt das lokale Modell direkt, ohne Tools (Routing einfach/normal/komplex).
    Ask { prompt: Vec<String> },
    /// Anmeldung bei Microsoft 365 (nur Lese-Berechtigungen).
    Login { provider: String },
    /// Speichert ein Geheimnis im macOS-Schlüsselbund (z. B. webuntis_password, brave_api_key).
    SetSecret { name: String },
    /// Zeigt das Audit-Log und prüft die Hash-Kette.
    Audit {
        #[arg(default_value_t = 20)]
        limit: usize,
    },
    /// Spricht einen Text (TTS-Test).
    Say { text: Vec<String> },
    /// Transkribiert eine WAV-Datei (STT-Test).
    Transcribe { wav: String },
}

/// Fragt im Terminal nach. Ohne Terminal: immer Nein.
struct TerminalConfirmer;

#[async_trait]
impl Confirmer for TerminalConfirmer {
    async fn confirm(&self, spec: &ToolSpec, _facts: &CallFacts, reason: &str) -> bool {
        eprint!("\n⚠️  Bestätigung nötig für '{}' ({:?}, Risiko {:?}):\n   {reason}\n   Ausführen? [j/N] ", spec.name, spec.access, spec.risk);
        let _ = std::io::stderr().flush();
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).is_ok() && matches!(s.trim().to_lowercase().as_str(), "j" | "ja" | "y" | "yes")
    }
}

/// Zeigt die Agent-Ereignisse im Terminal (kompakt oder ausführlich).
fn event_printer(verbose: bool) -> jarvis_agent::EventSink {
    use jarvis_agent::{AgentEvent, StepStatus};
    Arc::new(move |e| match e {
        AgentEvent::Understood { intent, complexity, mode, mode_reasons, model } if verbose => {
            let why = if mode_reasons.is_empty() { String::new() } else { format!(" ({})", mode_reasons.join(", ")) };
            eprintln!("  · verstanden: „{intent}“ · {complexity:?} · Modus {mode:?}{why} · {model}")
        }
        AgentEvent::Plan { text } => eprintln!("  · Plan:\n{}", text.lines().map(|l| format!("      {l}")).collect::<Vec<_>>().join("\n")),
        AgentEvent::ToolsSelected { tools } if verbose => eprintln!("  · Tools: {}", tools.join(", ")),
        AgentEvent::ToolCall { tool, args } => eprintln!("  → {tool} {args}"),
        AgentEvent::LlmCall { purpose, model, duration_ms, prompt_tokens, output_tokens, hidden_reasoning_chars, truncated, load_ms, prompt_ms, gen_ms, gpu_share } if verbose => {
            let mut extra = String::new();
            if gen_ms > 0 {
                extra.push_str(&format!(
                    "\n      Ollama: Laden {:.1} s · Prompt {:.1} s · Erzeugen {:.1} s = {:.1} Tokens/s",
                    load_ms as f64 / 1000.0,
                    prompt_ms as f64 / 1000.0,
                    gen_ms as f64 / 1000.0,
                    output_tokens as f64 * 1000.0 / gen_ms as f64
                ));
            }
            if let Some(g) = gpu_share {
                extra.push_str(&format!(" · GPU-Anteil {:.0} %", g * 100.0));
            }
            if hidden_reasoning_chars > 0 {
                extra.push_str(&format!(" · Denktext verworfen: {hidden_reasoning_chars} Zeichen"));
            }
            if truncated {
                extra.push_str(" · ABGESCHNITTEN (Längenlimit)");
            }
            eprintln!("  ⏱ Modell ({purpose}) {model}: {:.1} s · {prompt_tokens} Prompt- + {output_tokens} Antwort-Tokens{extra}", duration_ms as f64 / 1000.0)
        }
        AgentEvent::ToolResult { tool, status, summary, duration_ms } => {
            let icon = match status {
                StepStatus::Ok => "✓",
                StepStatus::NotConfirmed => "✋",
                StepStatus::Blocked => "⛔",
                _ => "✗",
            };
            eprintln!("    {icon} {tool} ({duration_ms} ms): {summary}")
        }
        AgentEvent::Verified { ok, detail, .. } if verbose => eprintln!("    {} geprüft: {detail}", if ok { "✓" } else { "✗" }),
        AgentEvent::Error { message } => eprintln!("  ✗ {message}"),
        AgentEvent::Phase { phase } if verbose => eprintln!("  [{phase:?}]"),
        _ => {}
    })
}

fn ok(label: &str, msg: impl std::fmt::Display) {
    println!("  ✅ {label:<22} {msg}");
}
fn warn(label: &str, msg: impl std::fmt::Display) {
    println!("  ⚠️  {label:<21} {msg}");
}
fn fail(label: &str, msg: impl std::fmt::Display) {
    println!("  ❌ {label:<22} {msg}");
}

fn which(bin: &str) -> Option<String> {
    let out = std::process::Command::new("sh").arg("-c").arg(format!("command -v {bin}")).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let dir = data_dir();
    if let Err(e) = run(cli.cmd, &dir).await {
        eprintln!("Fehler: {e}");
        std::process::exit(1);
    }
}

async fn run(cmd: Cmd, dir: &Path) -> Result<(), String> {
    match cmd {
        Cmd::Init => {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            let p = dir.join("config.toml");
            if p.exists() {
                println!("{} existiert bereits – nichts geändert.", p.display());
            } else {
                std::fs::write(&p, EXAMPLE_CONFIG).map_err(|e| e.to_string())?;
                println!("Angelegt: {}", p.display());
            }
            Ok(())
        }
        Cmd::Doctor { online } => doctor(dir, online).await,
        Cmd::Tools => {
            let app = build(dir, Arc::new(TerminalConfirmer))?;
            println!("{:<20} {:<11} {:<12} {:<8} {:<11} Fähigkeiten", "Tool", "Integration", "Zugriff", "Risiko", "Bestätigung");
            for t in app.gateway.registry().list() {
                let s = t.spec;
                println!(
                    "{:<20} {:<11} {:<12} {:<8} {:<11} {:?}",
                    s.name,
                    format!("{:?}", s.integration),
                    format!("{:?}", s.access),
                    format!("{:?}", s.risk),
                    format!("{:?}", s.confirmation),
                    s.capabilities
                );
            }
            for (i, why) in &app.inactive {
                println!("(inaktiv) {i}: {why}");
            }
            Ok(())
        }
        Cmd::Run { tool, args } => {
            let app = build(dir, Arc::new(TerminalConfirmer))?;
            let args: serde_json::Value = serde_json::from_str(&args).map_err(|e| format!("Argumente sind kein JSON: {e}"))?;
            let out = app.gateway.invoke(&tool, args, Origin::User).await.map_err(|e| e.to_string())?;
            println!("{}", out.text);
            app.gateway.services().unload_all_unused().await;
            Ok(())
        }
        Cmd::Agent { task, verbose } => {
            let app = build(dir, Arc::new(TerminalConfirmer))?;
            let agent = app.agent();
            let out = agent.run(&task.join(" "), event_printer(verbose)).await;
            println!("\n{}", out.answer);
            eprintln!("[{} · {} Schritte · {} + {} Tokens · {:.1}s]", out.model, out.steps.len(), out.prompt_tokens, out.output_tokens, out.duration_ms as f64 / 1000.0);
            if verbose {
                let other = out.duration_ms.saturating_sub(out.llm_ms + out.tool_ms);
                eprintln!(
                    "[Zeit: Modell {:.1} s ({} Aufrufe) · Tools {:.2} s · Rest (Laden, Memory, Logik) {:.2} s]",
                    out.llm_ms as f64 / 1000.0,
                    out.llm_calls,
                    out.tool_ms as f64 / 1000.0,
                    other as f64 / 1000.0
                );
            }
            app.gateway.services().unload_all_unused().await;
            Ok(())
        }
        Cmd::Chat { verbose } => {
            let app = build(dir, Arc::new(TerminalConfirmer))?;
            let agent = app.agent();
            println!("JARVIS bereit. Beenden mit 'exit', neuer Verlauf mit 'neu'.");
            loop {
                print!("\nDu › ");
                let _ = std::io::stdout().flush();
                let mut line = String::new();
                if std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                    break;
                }
                match line.trim() {
                    "" => continue,
                    "exit" | "quit" | "tschüss" => break,
                    "neu" => {
                        agent.reset().await;
                        println!("(Verlauf geleert)");
                        continue;
                    }
                    t => {
                        let out = agent.run(t, event_printer(verbose)).await;
                        println!("\nJARVIS › {}", out.answer);
                    }
                }
            }
            app.gateway.services().unload_all_unused().await;
            Ok(())
        }
        Cmd::Ask { prompt } => {
            let app = build(dir, Arc::new(TerminalConfirmer))?;
            let prompt = prompt.join(" ");
            let mut mon = Monitor::new();
            let mode = decide_mode(&mon.snapshot());
            let st = settings_for(mode);
            app.models.set_force_fallback(st.use_fallback_model, st.keep_alive_secs);
            let router = Router { strong_enabled: false, force_small: st.use_fallback_model };
            let c = router.classify(&prompt, 0);
            let tier = router.tier(c);
            eprintln!("[Modus {mode:?} · Aufgabe {c:?} · {tier:?} → {}]", app.models.model_for(tier));
            let guard = app.gateway.services().acquire("ollama").await?;
            let r = app
                .models
                .chat(tier, &[Message::new("system", "Du bist JARVIS, ein hilfsbereiter lokaler Assistent. Antworte knapp auf Deutsch."), Message::new("user", prompt)], &[])
                .await
                .map_err(|e| e.to_string())?;
            drop(guard);
            println!("{}", r.content.trim());
            eprintln!("[{} · {} Prompt-Tokens · {} Antwort-Tokens{}]", r.model, r.prompt_tokens, r.output_tokens, if r.used_fallback { " · Fallback" } else { "" });
            Ok(())
        }
        Cmd::Login { provider } => {
            if provider != "microsoft" {
                return Err("unterstützt: microsoft".into());
            }
            let cfg = load_config(dir)?;
            if cfg.microsoft.client_id.is_empty() {
                return Err("microsoft.client_id in config.toml fehlt (siehe TOOLS.md)".into());
            }
            let flow = jarvis_integrations::oauth::MsDeviceCodeFlow::new(cfg.microsoft.client_id.clone(), &cfg.microsoft.tenant);
            println!("Angefragte Berechtigungen (nur lesen): {:?}", jarvis_integrations::oauth::GRAPH_SCOPES);
            let code = flow.start().await.map_err(|e| e.to_string())?;
            println!("Öffne {} und gib den Code {} ein.", code.verification_uri, code.user_code);
            let refresh = flow.complete(&code).await.map_err(|e| e.to_string())?.ok_or("kein Refresh-Token erhalten")?;
            store_secret("ms_refresh_token", &refresh)?;
            println!("Angemeldet. Token liegt im macOS-Schlüsselbund (Dienst JARVIS).");
            Ok(())
        }
        Cmd::SetSecret { name } => {
            let allowed = ["webuntis_password", "brave_api_key", "gmail_access_token"];
            if !allowed.contains(&name.as_str()) {
                return Err(format!("erlaubt: {allowed:?}"));
            }
            if !cfg!(target_os = "macos") {
                return Err(format!("Schlüsselbund nur unter macOS; alternativ Umgebungsvariable JARVIS_{} setzen", name.to_uppercase()));
            }
            // `-w` ohne Wert: `security` fragt selbst verdeckt nach – das Geheimnis
            // erscheint so nie in der Prozessliste oder Shell-Historie.
            println!("Bitte den Wert für '{name}' zweimal eingeben (Eingabe bleibt unsichtbar):");
            let st = std::process::Command::new("security")
                .args(["add-generic-password", "-U", "-s", "JARVIS", "-a", &name, "-w"])
                .status()
                .map_err(|e| e.to_string())?;
            if !st.success() {
                return Err("Schlüsselbund-Eintrag fehlgeschlagen".into());
            }
            println!("Gespeichert im Schlüsselbund.");
            Ok(())
        }
        Cmd::Audit { limit } => {
            let app = build(dir, Arc::new(TerminalConfirmer))?;
            for e in app.audit.recent(limit).map_err(|e| e.to_string())?.into_iter().rev() {
                println!("#{:<5} {} {:<20} {:<6} {:<14} {}", e.id, e.ts, e.tool, e.origin, e.decision, e.outcome);
            }
            match app.audit.verify().map_err(|e| e.to_string())? {
                Ok(n) => println!("Hash-Kette intakt ({n} Einträge)."),
                Err(id) => println!("⚠️  Hash-Kette ab Eintrag #{id} beschädigt!"),
            }
            Ok(())
        }
        Cmd::Say { text } => {
            let cfg = load_config(dir)?;
            MacSay::new(cfg.voice.say_voice).speak(&text.join(" ")).await.map_err(|e| e.to_string())
        }
        Cmd::Transcribe { wav } => {
            let cfg = load_config(dir)?;
            let hw = jarvis_resources::detect_hardware();
            let p = model_profile(&cfg, &hw);
            let dir = expand(&cfg.voice.whisper_model_dir);
            let model = jarvis_voice::pick_stt_model(&dir, &[&p.stt_model, &p.stt_fallback])
                .ok_or_else(|| format!("kein Whisper-Modell in {} ({} oder {})", dir.display(), p.stt_model, p.stt_fallback))?;
            let t = WhisperCli::new(cfg.voice.whisper_binary, model).transcribe(Path::new(&wav)).await.map_err(|e| e.to_string())?;
            println!("{t}");
            Ok(())
        }
    }
}

async fn doctor(dir: &Path, online: bool) -> Result<(), String> {
    println!("JARVIS Doctor – Datenordner {}\n", dir.display());
    let app = build(dir, Arc::new(TerminalConfirmer))?;
    let hw = &app.hardware;
    let profile = app.models.profile();

    println!("Hardware");
    ok("System", format!("{} {} · {} · {} Kerne · {:.1} GB RAM", hw.os, hw.arch, hw.cpu_brand, hw.cpu_cores, hw.total_ram_gb));
    if hw.os == "macos" && !hw.apple_silicon {
        warn("Apple Silicon", "nicht erkannt (Rosetta?) – Metal-Beschleunigung fehlt");
    }
    let mut mon = Monitor::new();
    let snap = mon.snapshot();
    let mode = decide_mode(&snap);
    let decision = jarvis_resources::decide(&snap);
    ok(
        "Ressourcen",
        format!(
            "RAM frei (geschätzt) {:.1} GB · Speicherdruck {} · Akku {} · Low Power {} · Thermik {:?}",
            snap.available_ram_gb,
            snap.memory_pressure.map(|p| format!("{p:?}")).unwrap_or_else(|| "nicht lesbar".into()),
            snap.battery.map(|b| format!("{} % {}", b.percent, if b.charging { "(Netzteil)" } else { "(Akku)" })).unwrap_or_else(|| "–".into()),
            if snap.low_power_mode { "an" } else { "aus" },
            snap.thermal
        ),
    );
    let modeline = format!("{mode:?} – {} · kleines Modell erzwungen: {}", decision.reasons.join(", "), if decision.force_small_model() { "ja" } else { "nein" });
    if mode == jarvis_resources::Mode::Performance || mode == jarvis_resources::Mode::Balanced {
        ok("Modus", modeline);
    } else {
        warn("Modus", modeline);
    }
    if let Ok(out) = std::process::Command::new("df").args(["-g", "/"]).output() {
        let free = String::from_utf8_lossy(&out.stdout).lines().nth(1).and_then(|l| l.split_whitespace().nth(3).map(str::to_string)).unwrap_or_default();
        match free.parse::<u64>() {
            Ok(g) if g >= 30 => ok("SSD frei", format!("{g} GB")),
            Ok(g) => warn("SSD frei", format!("{g} GB – für alle Modelle werden ≥ 30 GB empfohlen")),
            _ => {}
        }
    }

    println!("\nLokales LLM (Ollama)");
    ok("Profil", format!("Haupt {} · Fallback {} · Embeddings {} · Kontext {} · KI-Budget {} GB", profile.main, profile.fallback, profile.embedding, profile.context_window, profile.ai_ram_budget_gb));
    match which("ollama") {
        Some(p) => ok("ollama", p),
        None => fail("ollama", "nicht installiert → scripts/setup-mac.sh"),
    }
    match app.models.load().await {
        Ok(()) => {
            ok("Server", app.models.client().version().await.unwrap_or_default());
            match app.models.missing_models().await {
                Ok(m) if m.is_empty() => ok("Modelle", "alle installiert"),
                Ok(m) => warn("Modelle", format!("fehlen: {} → scripts/setup-mac.sh", m.join(", "))),
                Err(e) => fail("Modelle", e),
            }
            if app.models.started_server() {
                ok("Ollama-Server", "von JARVIS gestartet (mit FLASH_ATTENTION, KV-Cache q8_0, 1 Modell)");
            } else {
                warn("Ollama-Server", "läuft extern (z. B. Ollama.app) – JARVIS-Einstellungen wie FLASH_ATTENTION gelten dort nicht");
            }
            // Jedes Modell einzeln messen: Geschwindigkeit, GPU-Anteil, Denkverhalten.
            let missing = app.models.missing_models().await.unwrap_or_default();
            let c = app.models.client();
            for (role, m) in [("Hauptmodell", &profile.main), ("Fallback", &profile.fallback)] {
                if missing.contains(m) {
                    continue;
                }
                let msgs = [Message::new("system", "Du bist ein Test."), Message::new("user", "Antworte nur mit dem Wort OK.")];
                match c.chat(m, &msgs, &[], 2048, 30, false).await {
                    Ok(r) => {
                        let tps = r.gen_tokens_per_s().map(|v| format!("{v:.1} Tokens/s")).unwrap_or_else(|| "? Tokens/s".into());
                        let gpu = c.gpu_share(m).await.map(|g| format!("{:.0} % GPU", g * 100.0)).unwrap_or_else(|| "GPU-Anteil ?".into());
                        let thinks = r.content.contains("</think>") || r.output_tokens > 40;
                        let line = format!(
                            "{m}: {} Tokens in {:.1} s ({tps}) · Laden {:.1} s · {gpu}{}",
                            r.output_tokens,
                            r.gen_ms as f64 / 1000.0,
                            r.load_ms as f64 / 1000.0,
                            if thinks { " · DENKT trotz think:false (reine Thinking-Variante?)" } else { "" }
                        );
                        if thinks || c.gpu_share(m).await.is_some_and(|g| g < 0.99) {
                            warn(role, line)
                        } else {
                            ok(role, line)
                        }
                    }
                    Err(e) => fail(role, e),
                }
                let _ = c.unload(m).await;
            }
            let _ = app.models.unload().await;
            ok("Entladen", "Modell und ggf. gestarteter Server wieder beendet");
        }
        Err(e) => fail("Server", e),
    }

    println!("\nMemory (SQLite)");
    app.memory.set_preference("doctor_check", "ok").map_err(|e| e.to_string())?;
    ok("brain.db", format!("Schema v{} · Lesen/Schreiben ok", app.memory.schema_version().map_err(|e| e.to_string())?));
    match app.audit.verify().map_err(|e| e.to_string())? {
        Ok(n) => ok("Audit-Log", format!("Hash-Kette intakt ({n} Einträge)")),
        Err(id) => fail("Audit-Log", format!("beschädigt ab #{id}")),
    }

    println!("\nDateisystem");
    for r in &app.config.filesystem.roots {
        let p = expand(r);
        if p.exists() {
            ok("Freigabe", p.display());
        } else {
            warn("Freigabe", format!("{} existiert nicht", p.display()));
        }
    }
    match app.gateway.invoke("fs_search", serde_json::json!({"pattern": "*.pdf", "limit": 3}), Origin::User).await {
        Ok(o) => ok("fs_search", o.text.lines().next().unwrap_or("")),
        Err(e) => warn("fs_search", format!("{e} (macOS: Terminal Zugriff auf Ordner erlauben)")),
    }

    println!("\nSicherheit");
    for a in ["send_email", "delete_email", "send_teams_message", "edit_teams_message", "modify_webuntis"] {
        match app.gateway.invoke(a, serde_json::json!({}), Origin::Agent).await {
            Err(jarvis_runtime::GatewayError::HardDenied(_)) => ok(a, "blockiert"),
            other => fail(a, format!("NICHT blockiert: {other:?}")),
        }
    }

    println!("\nVoice");
    let cfg = &app.config;
    match which(&cfg.voice.whisper_binary) {
        Some(p) => ok("whisper.cpp", p),
        None => warn("whisper.cpp", "nicht installiert → scripts/setup-mac.sh"),
    }
    let mdir = expand(&cfg.voice.whisper_model_dir);
    let mut stt = vec![&profile.stt_model];
    if profile.stt_fallback != profile.stt_model {
        stt.push(&profile.stt_fallback);
    }
    for m in stt {
        if mdir.join(m).exists() {
            ok("Whisper-Modell", m);
        } else {
            warn("Whisper-Modell", format!("{m} fehlt in {}", mdir.display()));
        }
    }
    match which("say") {
        Some(_) => {
            let out = std::process::Command::new("say").args(["-v", "?"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
            let de: Vec<&str> = out.lines().filter(|l| l.contains("de_")).filter_map(|l| l.split_whitespace().next()).collect();
            if de.iter().any(|v| *v == cfg.voice.say_voice) {
                ok("macOS TTS", format!("Stimme {} vorhanden", cfg.voice.say_voice))
            } else {
                warn("macOS TTS", format!("Stimme {} fehlt; deutsche Stimmen: {de:?}", cfg.voice.say_voice))
            }
        }
        None => warn("macOS TTS", "'say' nicht gefunden (kein macOS?)"),
    }

    println!("\nWeb / Recherche");
    ok("Suchanbieter", format!("{:?}", app.research.provider_ids()));
    if online {
        match app.gateway.invoke("web_search", serde_json::json!({"query": "Tauri Framework", "count": 3}), Origin::User).await {
            Ok(o) => ok("web_search", o.text.lines().next().unwrap_or("")),
            Err(e) => fail("web_search", e),
        }
    } else {
        warn("web_search", "übersprungen (mit --online testen)");
    }

    println!("\nRead-only-Integrationen");
    let names = app.gateway.registry().names();
    let probes = [("email", "email_recent", serde_json::json!({"limit": 3})), ("teams", "teams_chats", serde_json::json!({"limit": 3})), ("webuntis", "webuntis_timetable", serde_json::json!({}))];
    for (label, tool, args) in probes {
        if !names.contains(&tool) {
            let why = app.inactive.iter().find(|(i, _)| i == label).map(|(_, w)| w.as_str()).unwrap_or("inaktiv");
            warn(label, why);
        } else if online {
            match app.gateway.invoke(tool, args, Origin::User).await {
                Ok(o) => ok(label, format!("{} Zeilen gelesen", o.text.lines().count())),
                Err(e) => fail(label, e),
            }
        } else {
            ok(label, "konfiguriert (mit --online live testen)");
        }
    }
    println!("\nFertig. Details im Audit-Log: `jarvis audit`.");
    Ok(())
}
