# ARCHITECTURE.md

## Abhängigkeiten der Crates

```
jarvis-permissions        (keine Abhängigkeiten – unveränderliche Regeln)
   ▲
jarvis-runtime            Tool-Trait, Registry, Gateway, Lifecycle, Audit-/Confirm-Traits
   ▲            ▲
jarvis-memory   jarvis-context      jarvis-resources
   ▲     ▲          ▲      ▲              ▲
   │     └── jarvis-integrations          │
   │                │      jarvis-models ─┘
   │                │          ▲
   └────────── jarvis-cli (App-Zusammenbau, später Tauri) ── jarvis-voice
```

## Agent Core (`jarvis-agent`)

| Phase | Was passiert | Verbunden mit |
|---|---|---|
| **understand** | Ressourcenmodus lesen → Modellgröße/keep_alive setzen; Absicht (Intent-Schlüssel) und Komplexität bestimmen | Resource Manager, Context Router |
| **tool selection** | 3–8 passende Tools je Anfrage (deutsche Wortstämme, Tool-Familien, Erfolgsquoten, bewährte Workflows). Smalltalk → keine Tools, keine Schemas im Prompt | Tool Registry, Memory (`tool_stats`, `workflows`) |
| **plan** | nur bei komplexen Aufgaben: kurzer Plan (≤ 5 Schritte) als eigener Modellaufruf | Model Manager |
| **execute** | Tool-Calls des Modells (Ollama-Format, Fallback `<tool_call>`) → **ToolGateway** (Sperrliste → Policy → Bestätigung → Lifecycle → Audit). Nur für diese Anfrage ausgewählte Tools sind ausführbar | Gateway, Permission Layer, Lifecycle |
| **observe** | Ausgaben gekürzt (Token-Budget), Fehler mit bekannter Lösung aus Memory angereichert; Ablehnungen als „nicht erneut versuchen“ | Context Manager, Memory |
| **verify** | deterministische Nachbedingungen (Datei existiert / liegt im Papierkorb / wurde verschoben); bei Abweichung eine Korrekturrunde | Dateisystem |
| **finish** | Antwort + Pflicht-Hinweise zu blockierten/abgelehnten/fehlgeschlagenen Aktionen; Lernen (Workflow, Tool-Statistik, Fehler→Lösung); Verlauf komprimiert speichern; im Modus „Kritisch“ Dienste sofort entladen | Memory, Lifecycle |

Schutzmechanismen im Ablauf: Schrittlimit (8), max. 4 Tool-Calls pro Schritt,
doppelte Aufrufe werden übersprungen, Verlauf wird nie zwischen Tool-Aufruf
und Tool-Ergebnis abgeschnitten.

## Desktop-App (`app/`)

Tauri 2 + React 18 + TypeScript, ohne UI-Bibliotheken (klein, offline).
Die Rust-Seite (`app/src-tauri`) ist eine dünne Schicht über `jarvis_app::App`:

| Befehl / Event | Zweck |
|---|---|
| `send_message` | Agent-Lauf; immer nur einer gleichzeitig |
| `agent-event` (Event) | jede Phase, jeder Tool-Aufruf, jedes Ergebnis live in der Oberfläche |
| `confirm-request` / `confirm_response` | nativer Bestätigungsdialog (zufällige ID, 2 Min. Timeout = Nein) |
| `system_status` | Hardware, RAM/CPU/Akku, Modus, Modelle, Dienste (alle 5 s, nur bei sichtbarem Fenster) |
| `list_tools`, `audit_log` | Werkzeuge mit Rechten; Protokoll inkl. Hash-Ketten-Prüfung |
| `voice_start` / `voice_stop` | Push-to-Talk: native Aufnahme (cpal) → whisper.cpp → Transkript → `send_message` |
| `speak` / `stop_speaking` | Antwort vorlesen (macOS `say`), unterbrechbar |

Beim Schließen des Fensters werden alle Dienste (Ollama) entladen.

## Lifecycle

`ServiceManager` verwaltet ressourcenintensive Dienste (`ollama`, später
`whisper-server`, MCP-Subprozesse):

| Zustand | Bedeutung |
|---|---|
| OFF | nicht geladen, kein RAM |
| LOADING | wird gestartet (z. B. `ollama serve`) |
| READY | geladen, noch nicht genutzt |
| ACTIVE | mindestens ein Tool nutzt den Dienst (Guard) |
| IDLE | geladen, ungenutzt seit t |
| UNLOADING | wird beendet/entladen |
| ERROR | Laden/Entladen fehlgeschlagen; nächster Versuch lädt neu |

Ungültige Übergänge werden abgelehnt. Ein Reaper entlädt Dienste nach ihrem
Idle-Timeout. Bei Speicherdruck entlädt `unload_all_unused()` sofort.

## Ressourcenmodi

| Modus | Auslöser | Wirkung |
|---|---|---|
| Performance | Netzteil, genug RAM | Hauptmodell, keep_alive 300 s, Whisper turbo |
| Balanced | Akku | Hauptmodell, keep_alive 120 s, kein Wake Word |
| Saver | Akku < 40 %, Low Power, < 3 GB frei, Thermik „fair“ | Fallback-Modell, keep_alive 30 s, Whisper small |
| Critical | Akku < 15 %, < 1 GB frei, Thermik „serious“ | nur Text, Modell sofort entladen |

## Modellwahl nach RAM

| RAM | Haupt | Fallback | Kontext | KI-Budget |
|---|---|---|---|---|
| ≥ 30 GB | qwen3:14b | qwen3:8b | 16k | 14 GB |
| **16 GB (dein MacBook Air)** | **qwen3:8b (Q4_K_M)** | **qwen3:4b** | **8k** | **7 GB** |
| 8 GB | qwen3:4b | qwen3:1.7b | 4k | 3,5 GB |

Modelle lassen sich in `config.toml` überschreiben oder zur Laufzeit per
`ModelManager::set_profile` wechseln. Ollama läuft mit `OLLAMA_MAX_LOADED_MODELS=1`,
`OLLAMA_FLASH_ATTENTION=1` und `OLLAMA_KV_CACHE_TYPE=q8_0`.

## Nächste Phasen

1. Websuche-Netzwerkfehler auf dem Mac analysieren; E-Mail/Teams/WebUntis einrichten
2. Embeddings (sqlite-vec) für Memory und Tool-Retrieval
3. Gmail-Anmeldung, native Keychain-API
4. Workflows/Skills lernen, MCP-Brücke (optional)
5. Wake Word (openWakeWord), MLX-Backend (optional)
