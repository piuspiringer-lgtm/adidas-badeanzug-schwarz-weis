#!/usr/bin/env bash
# JARVIS – Rust-Installation prüfen, reparieren und die CLI bauen (macOS).
#
# 1. Diagnose (nur lesend): was ist installiert, was liegt im PATH?
# 2. Reparatur nur nach Rückfrage [j/N]. Es wird NICHTS gelöscht oder
#    deinstalliert; vorhandene Installationen werden wiederverwendet.
# 3. Build der JARVIS-CLI und `jarvis doctor`.
#
# Aufruf (im Ordner jarvis/):  bash scripts/fix-rust-mac.sh

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

bold() { printf '\n\033[1m%s\033[0m\n' "$1"; }
info() { printf '   %s\n' "$1"; }
have() { command -v "$1" >/dev/null 2>&1; }
ask()  { local a; read -r -p "   → $1 [j/N] " a </dev/tty; [[ "$a" =~ ^([jJyY]|ja|Ja)$ ]]; }

[[ "$(uname -s)" == "Darwin" ]] || { echo "Nur für macOS."; exit 1; }

# Homebrew auch finden, wenn es (noch) nicht im PATH der Shell ist.
if ! have brew; then
  for b in /opt/homebrew/bin/brew /usr/local/bin/brew; do [[ -x "$b" ]] && eval "$("$b" shellenv)" && break; done
fi

BREW_RUSTUP_BIN=""
if have brew && brew list --formula rustup >/dev/null 2>&1; then
  BREW_RUSTUP_BIN="$(brew --prefix rustup)/bin"
fi

# Ordner, in dem cargo/rustc/rustup liegen (rustup.rs-Installer oder Homebrew-rustup).
find_rust_bin() {
  local d
  for d in "$HOME/.cargo/bin" "$BREW_RUSTUP_BIN"; do
    [[ -n "$d" && -x "$d/rustup" ]] && { echo "$d"; return 0; }
  done
  have rustup && { dirname "$(command -v rustup)"; return 0; }
  return 1
}

bold "1. Diagnose"
info "Shell: $SHELL · Architektur: $(uname -m)"
for t in brew rustup rustup-init cargo rustc; do
  if have "$t"; then info "im PATH: $t → $(command -v "$t")"; else info "nicht im PATH: $t"; fi
done
[[ -d "$HOME/.cargo/bin" ]] && info "$HOME/.cargo/bin vorhanden: $(ls "$HOME/.cargo/bin" 2>/dev/null | tr '\n' ' ')"
[[ -n "$BREW_RUSTUP_BIN" ]] && info "Homebrew-Formel 'rustup' installiert (keg-only): $BREW_RUSTUP_BIN"
if have brew && brew list --formula rust >/dev/null 2>&1; then
  info "⚠️  Homebrew-Formel 'rust' ist ebenfalls installiert. Das kann mit rustup kollidieren."
  info "    Ich deinstalliere nichts. Die rustup-Toolchain wird im PATH bevorzugt."
fi
if [[ -d "$HOME/.rustup/toolchains" ]]; then
  info "installierte Toolchains: $(ls "$HOME/.rustup/toolchains" 2>/dev/null | tr '\n' ' ')"
fi
info "freier Speicher: $(df -g / | awk 'NR==2 {print $4}') GB (Build braucht ~2–4 GB)"

bold "2. rustup finden oder einrichten"
RUST_BIN="$(find_rust_bin || true)"
if [[ -z "$RUST_BIN" ]]; then
  info "Kein rustup gefunden."
  if have brew && ask "rustup über Homebrew installieren? (~10 MB, kostenlos; Toolchain danach ~1,5 GB)"; then
    brew install rustup || { info "brew install rustup fehlgeschlagen."; exit 1; }
    BREW_RUSTUP_BIN="$(brew --prefix rustup)/bin"
    RUST_BIN="$BREW_RUSTUP_BIN"
  else
    info "Abbruch – ohne rustup kann JARVIS nicht gebaut werden."; exit 1
  fi
fi
info "rustup: $RUST_BIN/rustup ($("$RUST_BIN/rustup" --version 2>/dev/null | head -1))"
export PATH="$RUST_BIN:$PATH"

bold "3. Toolchain"
if rustup default 2>/dev/null | grep -q stable; then
  info "✓ Standard-Toolchain: $(rustup default)"
else
  info "Keine Standard-Toolchain gesetzt (Ursache für 'cargo: command not found')."
  if ask "stabile Toolchain einrichten? (minimal, ~1,5 GB, kostenlos)"; then
    rustup set profile minimal
    rustup default stable || { info "rustup default stable fehlgeschlagen."; exit 1; }
  else
    exit 1
  fi
fi
rustup component list --installed 2>/dev/null | grep -q '^clippy' || info "(clippy nicht installiert – für den Build nicht nötig)"

# cargo kann je nach Installationsart in RUST_BIN oder ~/.cargo/bin liegen.
CARGO_BIN="$RUST_BIN"
[[ -x "$CARGO_BIN/cargo" ]] || CARGO_BIN="$HOME/.cargo/bin"
export PATH="$CARGO_BIN:$PATH"
if ! have cargo; then info "❌ cargo weiterhin nicht gefunden (gesucht in $RUST_BIN und ~/.cargo/bin)."; exit 1; fi
info "✓ $(cargo --version) · $(rustc --version)"

bold "4. PATH dauerhaft setzen"
PROFILE="$HOME/.zprofile"; [[ "$SHELL" == */bash ]] && PROFILE="$HOME/.bash_profile"
LINE="export PATH=\"$CARGO_BIN:\$PATH\"  # JARVIS: Rust"
if zsh -lc 'command -v cargo' >/dev/null 2>&1 || grep -qF "# JARVIS: Rust" "$PROFILE" 2>/dev/null; then
  info "✓ cargo ist in neuen Terminal-Fenstern verfügbar"
elif ask "Zeile an $PROFILE anhängen? ($LINE)"; then
  printf '\n%s\n' "$LINE" >> "$PROFILE"
  info "✓ angehängt (gilt für neue Terminal-Fenster)"
else
  info "übersprungen – in neuen Fenstern vorher: export PATH=\"$CARGO_BIN:\$PATH\""
fi

bold "5. JARVIS-CLI bauen"
if [[ -x target/release/jarvis ]]; then info "vorhanden: target/release/jarvis (wird inkrementell aktualisiert)"; fi
if ask "jetzt bauen? (cargo build --release -p jarvis-cli, erster Build ~3–6 min)"; then
  cargo build --release -p jarvis-cli || { info "❌ Build fehlgeschlagen – bitte die Ausgabe oben an Claude schicken."; exit 1; }
  info "✓ $(ls -lh target/release/jarvis | awk '{print $5}') · target/release/jarvis"
else
  exit 0
fi

bold "6. Doctor"
./target/release/jarvis init
./target/release/jarvis doctor
info ""
info "Online-Test (Websuche + konfigurierte Integrationen, nur lesend):"
info "  ./target/release/jarvis doctor --online"
info "Fehlen Ollama/Modelle/whisper.cpp: bash scripts/setup-mac.sh erneut starten (Vorhandenes wird übersprungen)."
