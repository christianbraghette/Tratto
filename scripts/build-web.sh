#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Christian Braghette
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Builds the web reader (crates/web) into crates/web/dist/, a static site:
# index.html, main.js, pkg/ (the WASM module and its bindings), fonts/,
# pdfium/ (PDFium's WASM build) and licenses/.
#
# Usage: scripts/build-web.sh [--serve]
#        --serve then serves it at http://localhost:8000 (needs Python 3).
#
# Needs the wasm32-unknown-unknown target and wasm-bindgen-cli of the same
# version as the wasm-bindgen in Cargo.lock (the script says which).
# PDFium is fetched into crates/web/pdfium/ when missing.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
web=$root/crates/web
dist=$web/dist
target_dir=${CARGO_TARGET_DIR:-$root/target}
# See crates/web/README.md.
pdfium_release=7902

bindgen_version=$(grep -A1 '^name = "wasm-bindgen"$' "$root/Cargo.lock" | sed -n 's/^version = "\(.*\)"/\1/p')
if ! rustup target list --installed | grep -qx wasm32-unknown-unknown; then
    echo "error: missing the WebAssembly target; run: rustup target add wasm32-unknown-unknown" >&2
    exit 1
fi
if [ "$(wasm-bindgen --version 2>/dev/null | cut -d' ' -f2)" != "$bindgen_version" ]; then
    echo "error: needs wasm-bindgen $bindgen_version; run: cargo install wasm-bindgen-cli --version $bindgen_version --locked" >&2
    exit 1
fi

pdfium_dir=$web/pdfium
if [ ! -f "$pdfium_dir/pdfium.wasm" ]; then
    echo "Fetching PDFium $pdfium_release (WASM)..."
    tmp=$(mktemp -d)
    curl -fsSL "https://github.com/paulocoutinhox/pdfium-lib/releases/download/$pdfium_release/wasm.tgz" -o "$tmp/wasm.tgz"
    tar -xzf "$tmp/wasm.tgz" -C "$tmp" release/node/pdfium.js release/node/pdfium.wasm
    mkdir -p "$pdfium_dir"
    cp "$tmp/release/node/pdfium.js" "$tmp/release/node/pdfium.wasm" "$pdfium_dir/"
    # The archive has no license: PDFium's, from its sources.
    curl -fsSL "https://pdfium.googlesource.com/pdfium/+/refs/heads/chromium/$pdfium_release/LICENSE?format=TEXT" |
        base64 -d > "$pdfium_dir/LICENSE"
    rm -rf "$tmp"
fi

cargo build --manifest-path "$root/Cargo.toml" -p tratto-web --target wasm32-unknown-unknown --release
# Emptied, not removed: a server may be serving it (Windows keeps it busy).
mkdir -p "$dist"
find "$dist" -mindepth 1 -delete
mkdir -p "$dist/fonts" "$dist/pdfium" "$dist/licenses"
wasm-bindgen --target web --no-typescript --out-dir "$dist/pkg" \
    "$target_dir/wasm32-unknown-unknown/release/tratto_web.wasm"
cp "$web/www/index.html" "$web/www/main.js" "$dist/"
cp "$root/crates/ui/assets/icon/icon.png" "$dist/"
cp "$root/crates/ui/assets/fonts/Inter-Regular.ttf" "$root/crates/ui/assets/fonts/Inter-SemiBold.ttf" "$dist/fonts/"
cp "$pdfium_dir/pdfium.js" "$pdfium_dir/pdfium.wasm" "$dist/pdfium/"
cp "$root/LICENSE" "$dist/licenses/LICENSE"
cp "$root/crates/ui/assets/fonts/OFL.txt" "$dist/licenses/Inter-OFL.txt"
cp "$pdfium_dir/LICENSE" "$dist/licenses/PDFium-LICENSE"
echo "Built $dist"

if [ "${1:-}" = "--serve" ]; then
    python=$(command -v python3 || command -v python)
    echo "Serving http://localhost:8000"
    "$python" -m http.server 8000 --directory "$dist"
fi
