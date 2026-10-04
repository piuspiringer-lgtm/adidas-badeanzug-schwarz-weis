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

## Datenfluss einer Anfrage (Zielbild Phase 1)

1. Eingabe (Text oder Push-to-Talk → whisper.cpp).
2. **Context Manager:** Aufgabe klassifizieren (einfach/normal/komplex) → Modellstufe. Der Ressourcenmodus kann das kleine Modell erzwingen.
3. **Tool-Vorauswahl:** wenige passende Tools (Memory: Erfolgsquoten, Workflows) statt aller Tools im Prompt.
4. **Model Manager:** Chat mit Tool-Calls (Ollama), Fallback bei Fehlern.
5. **Gateway:** Jeder Tool-Call durchläuft Sperrliste → Policy → Bestätigung → Lifecycle → Ausführung → Audit.
6. **Context Manager:** Tool-Ausgaben kürzen bzw. zusammenfassen, bevor sie zurück ins Modell gehen. Den Verlauf bei Bedarf komprimieren.
7. **Memory:** Erfolg oder Fehler, Dauer und Lösung speichern (Grundlage für Ranking).
8. Antwort → UI / TTS.

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

1. Agent-Kern (Planen → Tool-Calls → Reflektieren) + Tauri/React-UI mit Bestätigungsdialogen und Live-Audit
2. Embeddings (sqlite-vec) für Memory und Tool-Retrieval
3. Gmail-Anmeldung, native Keychain-API
4. Workflows/Skills lernen, MCP-Brücke (optional)
5. Wake Word (openWakeWord), MLX-Backend (optional)
