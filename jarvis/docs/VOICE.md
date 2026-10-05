# VOICE.md

| Teil | Lösung | Größe | Wann geladen |
|---|---|---|---|
| Spracherkennung | whisper.cpp (`whisper-cli`, Metal) mit `ggml-large-v3-turbo-q5_0.bin` | ~0,55 GB | nur während einer Transkription (eigener Prozess) |
| Fallback (Akku/Saver) | `ggml-small.bin` (mehrsprachig) | ~0,47 GB | dito |
| Sprachausgabe | macOS `say` mit Stimme „Anna“ (Premium empfohlen) | 0 | – |
| optional | Piper `de_DE-thorsten-medium` + `afplay` | ~80 MB | – |
| Aktivierung | **Push-to-Talk** (Hotkey); Wake Word später optional, standardmäßig aus | – | – |

Zustände: `Disabled → Idle → Recording → Transcribing → Idle`, `Idle → Speaking → Idle`.
Drückt man den Hotkey, während JARVIS spricht, wird die Ausgabe unterbrochen.

Sicherheit: Für das Modell gibt es kein Tool, das das Mikrofon einschaltet. Text
geht per stdin an `say`/Piper und wird nie als Kommandozeilenargument
interpretiert.

## In der Desktop-App (an den Agent Core angebunden)

1. **Sprechtaste** (Mikrofon-Symbol) gedrückt halten – oder **⌥ + Leertaste** halten.
   Der Reactor wird grün („Hört zu“). Spricht JARVIS gerade, wird er unterbrochen.
2. Loslassen → Aufnahme endet (max. 60 s), wird auf 16 kHz mono umgerechnet und
   **lokal** von whisper.cpp transkribiert („Versteht“). Die WAV-Datei wird sofort gelöscht.
3. Das Transkript geht als normale Anfrage an den Agenten – mit allen Regeln
   (Tool-Auswahl, Bestätigungen, Read-only, Audit).
4. Die Antwort wird vorgelesen (`say`, Stimme aus `config.toml`). Markdown und
   Links werden nicht vorgelesen; bei nicht ausgeführten Aktionen kommt ein kurzer Hinweis.

Modellwahl: das erste **vorhandene** Modell in der Reihenfolge des Profils
(Turbo → Small; im Modus „Sparen“ Small zuerst). Fehlt Turbo, wird Small genutzt –
es wird nichts automatisch heruntergeladen. Im Modus „Kritisch“ ist Sprache pausiert.

macOS fragt beim ersten Druck auf die Sprechtaste nach der Mikrofon-Berechtigung
(Begründung in `app/src-tauri/Info.plist`).

## Im Terminal

```bash
jarvis say "Hallo, ich bin JARVIS."
jarvis transcribe aufnahme.wav      # nutzt Turbo, sonst Small
```
