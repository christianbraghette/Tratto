#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Christian Braghette
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Builds the release archives the updater installs, `dist/tratto-<target>.zip`
# (the same ones the Release workflow attaches to a GitHub release):
# tratto(.exe), the PDFium library, LICENSE and licenses/.
#
# Usage: scripts/package.sh [windows] [linux] [macos]    (none: all three)
#
# Each platform is built where its toolchain is:
# * windows  on Windows (Git Bash).
# * linux    on Linux, or from Windows in WSL (run there once:
#              sudo apt install build-essential pkg-config libwayland-dev libxkbcommon-dev
#              curl https://sh.rustup.rs -sSf | sh -s -- -y
#            ).
# * macos    on a Mac only: it links against the Apple SDK. From Windows or
#            Linux, build it on GitHub instead: Actions > Release > Run
#            workflow (the archives are in the run's artifacts).
#
# PDFium is fetched into crates/ui/pdfium/ when missing.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
dist=$root/dist
# See crates/ui/pdfium/README.md.
pdfium_release=chromium%2F7881

host() {
    case "$(uname -s)" in
        MINGW* | MSYS* | CYGWIN*) echo windows ;;
        Linux) echo linux ;;
        Darwin) echo macos ;;
        *) echo unknown ;;
    esac
}

# platform -> target triple, PDFium archive, library in it, executable.
platform_info() {
    case $1 in
        windows) echo "x86_64-pc-windows-msvc win-x64 bin/pdfium.dll tratto.exe" ;;
        linux) echo "x86_64-unknown-linux-gnu linux-x64 lib/libpdfium.so tratto" ;;
        macos) echo "aarch64-apple-darwin mac-arm64 lib/libpdfium.dylib tratto" ;;
        *) echo "unknown platform: $1 (windows, linux or macos)" >&2; exit 2 ;;
    esac
}

# Zips the contents of folder $1 into $2 (absolute), keeping Unix permissions.
make_zip() {
    rm -f "$2"
    if [ "$(host)" = windows ]; then
        # Windows' own bsdtar writes zips (Git Bash's GNU tar does not).
        (cd "$1" && "$(cygpath -u "$SYSTEMROOT")/System32/tar.exe" -a -cf "$(cygpath -w "$2")" *)
    elif command -v zip > /dev/null; then
        (cd "$1" && zip -qr "$2" .)
    else
        (cd "$1" && python3 -m zipfile -c "$2" *)
    fi
}

build() {
    local platform=$1 target pdfium lib exe
    read -r target pdfium lib exe <<< "$(platform_info "$platform")"
    echo "==> $platform ($target)"

    local pdfium_dir=$root/crates/ui/pdfium/$pdfium
    if [ ! -f "$pdfium_dir/$lib" ]; then
        echo "    fetching PDFium ($pdfium)"
        mkdir -p "$pdfium_dir"
        curl -fsSL "https://github.com/bblanchon/pdfium-binaries/releases/download/$pdfium_release/pdfium-$pdfium.tgz" \
            | tar -xz -C "$pdfium_dir"
    fi

    if command -v rustup > /dev/null; then
        rustup target add "$target" > /dev/null
    fi
    (cd "$root" && cargo build --release --locked --target "$target")

    local target_dir=${CARGO_TARGET_DIR:-$root/target}
    local stage
    stage=$(mktemp -d)
    mkdir -p "$stage/licenses" "$dist"
    cp "$target_dir/$target/release/$exe" "$stage/"
    cp "$pdfium_dir/$lib" "$stage/"
    cp "$root/LICENSE" "$stage/"
    cp "$pdfium_dir/LICENSE" "$stage/licenses/PDFium-LICENSE"
    cp "$pdfium_dir"/licenses/* "$stage/licenses/"
    chmod 755 "$stage/$exe"
    make_zip "$stage" "$dist/tratto-$target.zip"
    rm -rf "$stage"
    echo "    dist/tratto-$target.zip"
}

# Windows: the Linux build runs in WSL, with its own target folder (faster
# than building on the Windows drive, and apart from the Windows build).
linux_in_wsl() {
    if ! command -v wsl.exe > /dev/null; then
        echo "==> linux: skipped (needs WSL on Windows)"
        return 1
    fi
    local wsl_root
    wsl_root=$(MSYS_NO_PATHCONV=1 wsl.exe -e wslpath -a "$(cygpath -w "$root")" | tr -d '\r')
    if ! wsl.exe -- bash -lc "command -v cargo > /dev/null && pkg-config --exists wayland-client"; then
        echo "==> linux: skipped, WSL is not set up for building; run once in WSL:"
        echo "      sudo apt install build-essential pkg-config libwayland-dev libxkbcommon-dev"
        echo "      curl https://sh.rustup.rs -sSf | sh -s -- -y"
        return 1
    fi
    wsl.exe -- bash -lc "cd '$wsl_root' && CARGO_TARGET_DIR=\$HOME/.cache/tratto/target bash scripts/package.sh linux"
}

# Platforms asked for by name must be built; with none, the others are
# only reported.
platforms=("$@")
[ ${#platforms[@]} -gt 0 ] || platforms=(windows linux macos)
strict=$#

here=$(host)
skipped=0
for platform in "${platforms[@]}"; do
    platform_info "$platform" > /dev/null
    if [ "$platform" = "$here" ]; then
        build "$platform"
    elif [ "$platform" = linux ] && [ "$here" = windows ]; then
        linux_in_wsl || skipped=1
    else
        echo "==> $platform: skipped (build it on $platform, or on GitHub: Actions > Release > Run workflow)"
        skipped=1
    fi
done
[ "$strict" -eq 0 ] || exit $skipped
