# JARVIS

Lokale, datenschutzfreundliche Desktop-KI für macOS (Apple Silicon).
Lokal-first, kostenlos und mit einem strikten Permission-System.

> **Status:** Phase 0 – Infrastruktur fertig und getestet (76 Tests).
> Die Desktop-App (Tauri + React) und der Agent-Kern folgen in Phase 1.

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
| CLI `jarvis` (doctor, tools, run, ask, login, audit, say, transcribe) | `jarvis-cli` |

## Schnellstart (Mac)

```bash
bash scripts/check-mac-env.sh      # nur lesen: Hardware & Tools
bash scripts/setup-mac.sh          # fragt jeden Schritt einzeln
bash scripts/fix-rust-mac.sh       # falls `cargo` fehlt: Rust prüfen/reparieren, CLI bauen, Doctor
./target/release/jarvis doctor     # alles prüfen
./target/release/jarvis ask "Was kannst du?"
./target/release/jarvis run fs_search '{"pattern":"*.pdf"}'
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
cargo test --workspace
cargo clippy --workspace --all-targets
```
