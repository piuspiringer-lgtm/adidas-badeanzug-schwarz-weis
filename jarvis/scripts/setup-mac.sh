#!/usr/bin/env bash
# JARVIS – Einrichtung auf macOS (Apple Silicon).
#
# * Jeder Schritt zeigt Zweck, Größe, Kosten und Berechtigungen und fragt
#   VORHER nach [j/N]. Ohne "j" wird nichts installiert.
# * Bereits Vorhandenes wird erkannt und übersprungen.
# * Keine Hintergrunddienste: Ollama wird NICHT als Autostart-Dienst
#   eingerichtet – JARVIS startet und beendet es bei Bedarf selbst.
#
# Aufruf:  bash scripts/setup-mac.sh

set -euo pipefail
cd "$(dirname "$0")/.."

DATA_DIR="$HOME/Library/Application Support/JARVIS"
WHISPER_DIR="$DATA_DIR/models/whisper"
HF="https://huggingface.co/ggerganov/whisper.cpp/resolve/main"

bold() { printf '\n\033[1m%s\033[0m\n' "$1"; }
info() { printf '   %s\n' "$1"; }
have() { command -v "$1" >/dev/null 2>&1; }
ask()  { local a; read -r -p "   → $1 [j/N] " a </dev/tty; [[ "$a" =~ ^([jJyY]|ja|Ja)$ ]]; }

[[ "$(uname -s)" == "Darwin" ]] || { echo "Nur für macOS."; exit 1; }

bold "0. System"
ARCH=$(uname -m)
RAM_GB=$(( $(sysctl -n hw.memsize) / 1024 / 1024 / 1024 ))
FREE_GB=$(df -g / | awk 'NR==2 {print $4}')
CHIP=$(sysctl -n machdep.cpu.brand_string)
info "Chip: $CHIP ($ARCH) · RAM: ${RAM_GB} GB · frei: ${FREE_GB} GB · macOS $(sw_vers -productVersion)"
[[ "$ARCH" == "arm64" ]] || info "⚠️  Kein Apple Silicon erkannt – lokale KI wird langsam sein."
if (( FREE_GB < 30 )); then
  info "⚠️  Weniger als 30 GB frei. Empfehlung: zuerst nur das kleine Modell installieren."
fi

if   (( RAM_GB >= 30 )); then MAIN=qwen3:14b; FALLBACK=qwen3:8b;   MAIN_GB=9.3; FB_GB=5.2
elif (( RAM_GB >= 15 )); then MAIN=qwen3:8b;  FALLBACK=qwen3:4b;   MAIN_GB=5.2; FB_GB=2.5
elif (( RAM_GB >= 8 ));  then MAIN=qwen3:4b;  FALLBACK=qwen3:1.7b; MAIN_GB=2.5; FB_GB=1.4
else                          MAIN=qwen3:1.7b; FALLBACK=qwen3:0.6b; MAIN_GB=1.4; FB_GB=0.5
fi
EMBED=nomic-embed-text
if [[ "$ARCH" == "arm64" ]] && (( RAM_GB >= 15 )); then STT=ggml-large-v3-turbo-q5_0.bin; else STT=ggml-small.bin; fi
info "Modellauswahl: Haupt $MAIN (~${MAIN_GB} GB) · Fallback $FALLBACK (~${FB_GB} GB) · Embeddings $EMBED (~0,3 GB) · STT $STT"

bold "1. Xcode Command Line Tools (Compiler für Rust/Tauri)"
info "Größe ~1,5–2 GB · kostenlos · Apple · volles Xcode wird NICHT benötigt"
if xcode-select -p >/dev/null 2>&1; then info "✓ vorhanden"; elif ask "installieren?"; then xcode-select --install || true; info "Dialog abschließen und Skript danach erneut starten."; exit 0; fi

bold "2. Homebrew (Paketmanager)"
info "Größe ~0,5 GB · kostenlos · Open Source · Installation nach /opt/homebrew (Admin-Passwort nötig)"
if have brew; then info "✓ vorhanden"; elif ask "offiziellen Installer von brew.sh ausführen?"; then
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  eval "$(/opt/homebrew/bin/brew shellenv)"
fi

bold "3. Rust + Node.js (Entwicklung)"
info "Rust ~1,5 GB (+ Build-Ordner 3–8 GB) · Node ~0,2 GB · kostenlos"
if have cargo; then info "✓ Rust $(rustc --version | cut -d' ' -f2)"; elif have brew && ask "rustup über Homebrew installieren?"; then
  brew install rustup && rustup-init -y --profile minimal && source "$HOME/.cargo/env"
fi
if have node; then info "✓ Node $(node --version)"; elif have brew && ask "Node.js (LTS) installieren? (erst für die UI-Phase nötig)"; then brew install node; fi

bold "4. Ollama (lokale LLM-Runtime, Metal-beschleunigt)"
info "~0,1 GB Programm · kostenlos · Open Source · lauscht nur auf 127.0.0.1:11434"
info "KEIN Autostart: JARVIS startet 'ollama serve' nur bei Bedarf und beendet es wieder."
if have ollama; then info "✓ $(ollama --version 2>/dev/null | tail -1)"; elif have brew && ask "Ollama über Homebrew installieren?"; then brew install ollama; fi

if have ollama; then
  bold "5. Sprachmodelle (Download aus der Ollama-Bibliothek)"
  STARTED=0
  if ! curl -s -m 2 http://127.0.0.1:11434/api/version >/dev/null; then
    OLLAMA_HOST=127.0.0.1:11434 ollama serve >/dev/null 2>&1 & OLLAMA_PID=$!; STARTED=1
    for _ in $(seq 1 30); do curl -s -m 1 http://127.0.0.1:11434/api/version >/dev/null && break; sleep 0.5; done
  fi
  MODELS=("$FALLBACK" "$MAIN" "$EMBED")
  SIZES=("$FB_GB" "$MAIN_GB" "0.3")
  LABELS=("Fallback (klein, schnell, Akku-Modus)" "Hauptmodell" "Embeddings für Memory")
  for i in 0 1 2; do
    model=${MODELS[$i]}
    if ollama list 2>/dev/null | awk '{print $1}' | grep -qx -e "$model" -e "$model:latest"; then info "✓ $model vorhanden"
    elif ask "$model laden? (${LABELS[$i]}, ~${SIZES[$i]} GB, kostenlos)"; then ollama pull "$model"; fi
  done
  (( STARTED )) && kill "$OLLAMA_PID" 2>/dev/null || true
fi

bold "6. whisper.cpp (lokale Spracherkennung, Metal)"
info "Programm ~10 MB · kostenlos · Open Source (MIT) · Mikrofonzugriff erst in der App, nur per Push-to-Talk"
if have whisper-cli; then info "✓ vorhanden"; elif have brew && ask "whisper-cpp über Homebrew installieren?"; then brew install whisper-cpp; fi
mkdir -p "$WHISPER_DIR"
for m in "$STT" ggml-small.bin; do
  if [[ -f "$WHISPER_DIR/$m" ]]; then info "✓ $m vorhanden"; continue; fi
  size=$([[ $m == *turbo* ]] && echo "~0,55 GB" || echo "~0,47 GB")
  if ask "Whisper-Modell $m laden? ($size, von huggingface.co/ggerganov, kostenlos)"; then
    curl -fL --progress-bar -o "$WHISPER_DIR/$m.part" "$HF/$m" && mv "$WHISPER_DIR/$m.part" "$WHISPER_DIR/$m"
  fi
  [[ "$m" == "ggml-small.bin" ]] && break
done

bold "7. Deutsche Systemstimme (Text-to-Speech)"
if say -v '?' | grep -q '^Anna'; then info "✓ Stimme 'Anna' vorhanden"; fi
info "Für bessere Qualität (kostenlos, ~0,2–0,5 GB, manuell):"
info "Systemeinstellungen → Bedienungshilfen → Gesprochene Inhalte → Systemstimme → Stimmen verwalten → Deutsch → 'Anna (Premium)'"

bold "8. JARVIS-CLI bauen und prüfen"
if have cargo && ask "jarvis (Release) bauen? (~3–5 min, ~2 GB Build-Ordner)"; then
  cargo build --release -p jarvis-cli
  ./target/release/jarvis init
  ./target/release/jarvis doctor
fi

bold "Fertig."
info "Nächste Schritte (optional): siehe TOOLS.md → E-Mail/Teams/WebUntis einrichten."
