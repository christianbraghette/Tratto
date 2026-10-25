#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Christian Braghette
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Builds the web app (the desktop app's UI crate for the browser, see
# crates/ui/src/web.rs) into crates/ui/web/dist/, a static site.
#
# Usage: scripts/build-web-app.sh [--serve]
#        --serve then serves it at http://localhost:8001 (needs Python 3).
#
# Needs what scripts/build-web.sh needs (the wasm32-unknown-unknown target,
# wasm-bindgen-cli) and uses the PDFium it fetches (crates/web/pdfium/).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
web=$root/crates/ui/web
dist=$web/dist
target_dir=${CARGO_TARGET_DIR:-$root/target}
pdfium_dir=$root/crates/web/pdfium

if [ ! -f "$pdfium_dir/pdfium.wasm" ]; then
    echo "note: no PDFium in $pdfium_dir (run scripts/build-web.sh once): PDFs will not show" >&2
fi

cargo build --manifest-path "$root/Cargo.toml" -p tratto-ui --target wasm32-unknown-unknown --release
# Emptied, not removed: a server may be serving it (Windows keeps it busy).
mkdir -p "$dist"
find "$dist" -mindepth 1 -delete
mkdir -p "$dist/licenses"
wasm-bindgen --target web --no-typescript --out-dir "$dist/pkg" \
    "$target_dir/wasm32-unknown-unknown/release/tratto.wasm"
cp "$web/index.html" "$dist/"
cp "$root/crates/ui/assets/icon/icon.png" "$dist/"
cp "$root/LICENSE" "$dist/licenses/LICENSE"
cp "$root/crates/ui/assets/fonts/OFL.txt" "$dist/licenses/Inter-OFL.txt"
if [ -f "$pdfium_dir/pdfium.wasm" ]; then
    mkdir -p "$dist/pdfium"
    cp "$pdfium_dir/pdfium.js" "$pdfium_dir/pdfium.wasm" "$dist/pdfium/"
    cp "$pdfium_dir/LICENSE" "$dist/licenses/PDFium-LICENSE"
fi
echo "Built $dist"

if [ "${1:-}" = "--serve" ]; then
    python=$(command -v python3 || command -v python)
    echo "Serving http://localhost:8001"
    "$python" -m http.server 8001 --directory "$dist"
fi
