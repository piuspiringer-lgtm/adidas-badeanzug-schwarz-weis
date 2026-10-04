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

Testen auf dem Mac:

```bash
jarvis say "Hallo, ich bin JARVIS."
# Aufnahme (16 kHz mono) z. B. mit: ffmpeg -f avfoundation -i ":0" -ar 16000 -ac 1 -t 5 test.wav
jarvis transcribe test.wav
```

Die Audioaufnahme selbst (Mikrofon → WAV) kommt mit der Tauri-UI (Phase 1).
