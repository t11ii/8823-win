#!/usr/bin/env bash
# Render the app icon SVG into icon.ico, AppIcon.icns and the runtime PNGs.
# usage: make.sh <icon.svg> <outdir>
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
S=$(realpath "$1"); O=$(realpath "$2"); T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
# Windows: tighter crop so the small taskbar sizes are filled better.
sed 's#viewBox="0 0 1024 1024"#viewBox="76 76 872 872"#' "$S" > "$T/win.svg"
args=()
for n in 16 20 24 32 40 48 64 128 256; do args+=("$T/win.svg" "$T/w$n.png" $n); done
for n in 16 32 64 128 256 512 1024; do args+=("$S" "$T/m$n.png" $n); done
node "$HERE/render.js" "${args[@]}"
convert "$T"/w{16,20,24,32,40,48,64,128,256}.png "$T/im.ico"
python3 "$HERE/ico.py" "$T/im.ico" "$T" "$O/icon.ico"
python3 "$HERE/icns.py" "$T" "$O/AppIcon.icns"
cp "$T/w256.png" "$O/icon-256.png"; cp "$T/m512.png" "$O/icon-512.png"
