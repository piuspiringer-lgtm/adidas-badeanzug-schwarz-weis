# JARVIS

Lokale, datenschutzfreundliche Desktop-KI für macOS (Apple Silicon).
Lokal-first, kostenlos und mit einem strikten Permission-System.

> **Status:** Agent Core und Desktop-App laufen. Voice-Anbindung an den
> Agenten ist der nächste Schritt.

## Was schon da ist

| Bereich | Crate |
|---|---|
| Permission-Schicht (Policy, Sperrliste, Validierung) | `jarvis-permissions` |
| Tool-Gateway, Registry, Service-Lifecycle (OFF→LOADING→READY→ACTIVE→IDLE→UNLOADING, ERROR) | `jarvis-runtime` |
| SQLite-Memory, Research-Cache, Audit-Log mit Hash-Kette | `jarvis-memory` |
| Context/Token Efficiency Manager (Budget, Chunking, BM25, Duplikate, Verlauf komprimieren, Modell-Routing) | `jarvis-context` |
| Resource Manager (Hardware, RAM, Akku, Thermik → Modus und Modellwahl) | `jarvis-resources` |
| Model Manager (Ollama, Fallback, immer nur ein Modell geladen) | `jarvis-models` |
| Dateisystem, Web-Recherche, E-Mail, Teams, WebUntis | `jarvis-integrations` |
| Spracherkennung (whisper.cpp), Sprachausgabe (macOS/Piper), Push-to-Talk | `jarvis-voice` |
| **Agent Core**: understand → plan → tool selection → execute → observe → verify → finish | `jarvis-agent` |
| CLI `jarvis` (chat, agent, doctor, tools, run, ask, login, audit, say, transcribe) | `jarvis-cli` |
| **Desktop-App** (Tauri + React): Chat, Arc-Reactor-Status, Live-Aktivität, Bestätigungsdialog, Werkzeuge, Protokoll | `app/` |

## Schnellstart (Mac)

```bash
bash scripts/check-mac-env.sh      # nur lesen: Hardware & Tools
bash scripts/setup-mac.sh          # fragt jeden Schritt einzeln
bash scripts/fix-rust-mac.sh       # falls `cargo` fehlt: Rust prüfen/reparieren, CLI bauen, Doctor
./target/release/jarvis doctor     # alles prüfen
./target/release/jarvis chat                     # Dialog mit dem Agenten im Terminal
./target/release/jarvis agent -v "Finde meine PDFs in Dokumente"
```

### Desktop-App

```bash
cd app
npm install                 # einmalig, ~120 MB
npm run tauri dev           # Entwicklung (Hot Reload)
npm run tauri build         # fertige JARVIS.app + .dmg in target/release/bundle/
```

## Dokumentation

- [TOOLS.md](TOOLS.md) – alle Integrationen, Berechtigungen, Kosten, Einrichtung
- [SECURITY.md](SECURITY.md) – Sicherheitsmodell und Nachweise
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) – Aufbau und Datenflüsse
- [docs/MEMORY.md](docs/MEMORY.md) – Datenbankschema und Lernen
- [docs/VOICE.md](docs/VOICE.md) – Sprache
- [docs/PLAN.md](docs/PLAN.md) – ursprünglicher Plan und Roadmap

## Entwicklung

```bash
cargo test --workspace                 # Rust (inkl. Agent- und Sicherheitstests)
cargo clippy --workspace --all-targets
cd app && npm test                     # Oberfläche
app/e2e/run.sh                         # E2E der echten App (Linux/CI, tauri-driver)
```
