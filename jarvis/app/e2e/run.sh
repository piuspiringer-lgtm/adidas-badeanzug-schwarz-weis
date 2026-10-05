#!/usr/bin/env bash
# Startet Fake-Ollama, tauri-driver (unter Xvfb) und den E2E-Test der echten App.
# Voraussetzungen (Linux): webkit2gtk-driver, xvfb, tauri-driver, python3 + selenium.
set -euo pipefail
cd "$(dirname "$0")"
APP="${1:-../../target/debug/jarvis-desktop}"
OUT="${2:-./screenshots}"
PY="${PYTHON:-python3}"
JARVIS_DATA_DIR="$(mktemp -d)"
export JARVIS_DATA_DIR
mkdir -p "$JARVIS_DATA_DIR" "$HOME/Documents"
printf '[models]\nollama_url = "http://127.0.0.1:11556"\n[filesystem]\nroots = ["~/Documents"]\n' > "$JARVIS_DATA_DIR/config.toml"
"$PY" fake_ollama.py 11556 & FAKE=$!
xvfb-run -a -s "-screen 0 1400x900x24" tauri-driver --port 4444 >/dev/null 2>&1 & DRV=$!
# Aufräumen, ohne den Exit-Code des Tests zu verändern.
cleanup() {
  local status=$?
  set +e
  kill "$FAKE" "$DRV" 2>/dev/null
  pkill -f "tauri-driver --port 4444" 2>/dev/null
  pkill -f WebKitWebDriver 2>/dev/null
  exit "$status"
}
trap cleanup EXIT
for _ in $(seq 1 40); do curl -s http://127.0.0.1:4444/status >/dev/null && break; sleep 0.25; done
"$PY" e2e.py "$(realpath "$APP")" "$OUT"
