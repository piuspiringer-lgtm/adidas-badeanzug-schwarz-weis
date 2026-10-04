#!/usr/bin/env bash
# JARVIS – Umgebungsanalyse für macOS.
# NUR LESEND: installiert nichts, ändert nichts, sendet nichts ins Netz.
# Aufruf:  bash scripts/check-mac-env.sh | tee jarvis-env-report.txt

set -u

section() { printf '\n===== %s =====\n' "$1"; }
have()    { command -v "$1" >/dev/null 2>&1; }
ver()     { if have "$1"; then printf '%-14s %s\n' "$1" "$("$@" 2>&1 | head -1)"; else printf '%-14s %s\n' "$1" "NICHT INSTALLIERT"; fi; }

if [ "$(uname -s)" != "Darwin" ]; then
  echo "Dieses Skript ist für macOS gedacht (gefunden: $(uname -s))." >&2
  exit 1
fi

section "macOS"
sw_vers
uname -m

section "Chip / CPU"
sysctl -n machdep.cpu.brand_string
printf 'Performance-Kerne: %s\n' "$(sysctl -n hw.perflevel0.physicalcpu 2>/dev/null || echo ?)"
printf 'Effizienz-Kerne:   %s\n' "$(sysctl -n hw.perflevel1.physicalcpu 2>/dev/null || echo ?)"
printf 'Rosetta aktiv für diese Shell: %s\n' "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)"

section "RAM"
printf 'RAM gesamt: %s GB\n' "$(( $(sysctl -n hw.memsize) / 1024 / 1024 / 1024 ))"
memory_pressure 2>/dev/null | tail -1
vm_stat | head -6
sysctl -n vm.swapusage

section "GPU / Metal / Neural Engine"
system_profiler SPDisplaysDataType 2>/dev/null | grep -E 'Chipset Model|Total Number of Cores|Metal' || true
system_profiler SPHardwareDataType 2>/dev/null | grep -E 'Model Name|Model Identifier|Chip|Memory|Total Number of Cores' || true

section "Speicher"
df -h / /System/Volumes/Data 2>/dev/null
for d in "$HOME/.ollama" "$HOME/.cache/huggingface" "$HOME/Library/Caches" "$HOME/.cargo" "$HOME/.rustup" "$HOME/Library/Developer"; do
  [ -d "$d" ] && printf '%-40s %s\n' "$d" "$(du -sh "$d" 2>/dev/null | cut -f1)"
done

section "Akku / Energie"
pmset -g batt
pmset -g therm 2>/dev/null | head -5
printf 'Low Power Mode: %s\n' "$(pmset -g | awk '/lowpowermode/ {print $2}')"

section "Entwicklungswerkzeuge"
printf '%-14s %s\n' "xcode-select" "$(xcode-select -p 2>&1)"
ver xcodebuild -version
ver brew --version
ver git --version
ver node --version
ver npm --version
ver pnpm --version
ver bun --version
ver rustc --version
ver cargo --version
ver rustup --version
ver python3 --version
ver uv --version
ver cmake --version
ver ffmpeg -version
ver sqlite3 --version
ver docker --version

section "Lokale KI / Voice"
ver ollama --version
have ollama && ollama list 2>/dev/null
ver llama-cli --version
ver whisper-cli --help
python3 -c 'import mlx.core as mx; print("mlx", mx.__version__)' 2>/dev/null || echo "mlx           NICHT INSTALLIERT"
ver piper --help
echo "macOS-Stimmen (Deutsch):"
say -v '?' 2>/dev/null | grep -i 'de_' || echo "  keine gefunden"

section "Fertig"
echo "Bitte die komplette Ausgabe an Claude zurückgeben."
