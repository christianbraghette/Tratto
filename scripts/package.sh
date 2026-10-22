#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Christian Braghette
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Builds what a release offers for download, in dist/ (the same files the
# Release workflow attaches to a GitHub release):
# * windows  Tratto-x64.msi, the installer; tratto-<target>.zip, the
#            portable app, also what the updater installs.
# * linux    Tratto-x86_64.AppImage, also what the updater of an AppImage
#            installs; tratto-<target>.zip, the portable app.
# * macos    Tratto-arm64.dmg, holding Tratto.app; tratto-<target>.app.zip,
#            what the updater of Tratto.app installs; tratto-<target>.zip,
#            the bare app (for the updater of installations made from it).
# A zip holds tratto(.exe), the PDFium library, LICENSE and licenses/.
#
# Usage: scripts/package.sh [--build-only | --bundle-only] [windows] [linux] [macos]
#        (no platform: all three)
#
# --build-only stops after compiling, --bundle-only starts from what was
# compiled: the Release workflow signs the executable in between.
#
# Each platform is built where its toolchain is:
# * windows  on Windows (Git Bash). The installer is made with WiX, fetched
#            (needs the .NET runtime, 6 or later).
# * linux    on Linux, or from Windows in WSL (run there once:
#              sudo apt install build-essential pkg-config libwayland-dev libxkbcommon-dev
#              curl https://sh.rustup.rs -sSf | sh -s -- -y
#            ). The AppImage is made with appimagetool, fetched.
# * macos    on a Mac only: it links against the Apple SDK. From Windows or
#            Linux, build it on GitHub instead: Actions > Release > Run
#            workflow (the files are in the run's artifacts).
#
# Signing, when set up (else the files are unsigned, macOS ad hoc):
# * TRATTO_SIGN_CERT  Windows: the SHA-1 thumbprint of a code signing
#                     certificate in the user's store, signed with by signtool.
# * TRATTO_MACOS_IDENTITY  macOS: the "Developer ID Application: ..."
#                     identity in the keychain. With APPLE_ID, APPLE_TEAM_ID
#                     and APPLE_APP_PASSWORD (an app-specific password) the
#                     .dmg is also notarized by Apple.
#
# PDFium is fetched into crates/ui/pdfium/ when missing.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
dist=$root/dist
target_dir=${CARGO_TARGET_DIR:-$root/target}
# See crates/ui/pdfium/README.md.
pdfium_release=chromium%2F7881
wix_version=5.0.2
appimagetool_version=1.9.1
version=$(grep -m1 '^version = ' "$root/Cargo.toml" | cut -d'"' -f2)

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
        (cd "$1" && zip -qry "$2" .)
    else
        (cd "$1" && python3 -m zipfile -c "$2" *)
    fi
}

compile() {
    local platform=$1 target pdfium lib exe
    read -r target pdfium lib exe <<< "$(platform_info "$platform")"
    echo "==> $platform ($target): compiling"

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
}

bundle() {
    local platform=$1 target pdfium lib exe
    read -r target pdfium lib exe <<< "$(platform_info "$platform")"
    echo "==> $platform ($target): packaging"
    local pdfium_dir=$root/crates/ui/pdfium/$pdfium

    # The portable app, unpacked: what the zip holds and the installers
    # are made from.
    local work stage
    work=$(mktemp -d)
    stage=$work/stage
    mkdir -p "$stage/licenses" "$dist"
    cp "$target_dir/$target/release/$exe" "$stage/"
    cp "$pdfium_dir/$lib" "$stage/"
    cp "$root/LICENSE" "$stage/"
    cp "$pdfium_dir/LICENSE" "$stage/licenses/PDFium-LICENSE"
    cp "$pdfium_dir"/licenses/* "$stage/licenses/"
    chmod 755 "$stage/$exe"
    [ "$platform" != windows ] || sign_windows "$stage/$exe"
    make_zip "$stage" "$dist/tratto-$target.zip"
    echo "    dist/tratto-$target.zip"

    case $platform in
        windows) windows_msi "$stage" "$work" ;;
        linux) linux_appimage "$stage" "$work" ;;
        macos) macos_app "$stage" "$work" "$target" ;;
    esac
    rm -rf "$work"
}

# Signs the Windows file $1 with the certificate TRATTO_SIGN_CERT, if set.
sign_windows() {
    [ -n "${TRATTO_SIGN_CERT:-}" ] || return 0
    local signtool
    signtool=$(ls "/c/Program Files (x86)/Windows Kits/10/bin/"*/x64/signtool.exe 2> /dev/null | sort -V | tail -1)
    if [ -z "$signtool" ]; then
        echo "signtool not found: install the Windows SDK" >&2
        exit 1
    fi
    MSYS_NO_PATHCONV=1 "$signtool" sign /sha1 "$TRATTO_SIGN_CERT" /fd sha256 /td sha256 \
        /tr http://timestamp.digicert.com /d Tratto "$(cygpath -w "$1")"
}

# The installer, from the portable app in $1 (see packaging/windows/tratto.wxs).
windows_msi() {
    local stage=$1 work=$2
    # WiX (its NuGet package, run on whichever .NET runtime is installed)
    # and its extensions go to a folder of their own; `wix` finds the
    # extensions from there.
    local tools=$target_dir/tools/wix-$wix_version
    export DOTNET_ROLL_FORWARD=Major
    if [ ! -x "$tools/wix.exe" ]; then
        echo "    fetching WiX $wix_version"
        rm -rf "$tools.part"
        mkdir -p "$tools.part"
        curl -fsSL -o "$tools.part/wix.nupkg" "https://www.nuget.org/api/v2/package/wix/$wix_version"
        "$(cygpath -u "$SYSTEMROOT")/System32/tar.exe" -xf "$(cygpath -w "$tools.part/wix.nupkg")"             -C "$(cygpath -w "$tools.part")" tools/net6.0/any
        mv "$tools.part/tools/net6.0/any" "$tools"
        rm -rf "$tools.part"
        (cd "$tools" && ./wix.exe extension add "WixToolset.UI.wixext/$wix_version" "WixToolset.Util.wixext/$wix_version")
    fi
    # The license page: LICENSE as RTF.
    {
        printf '{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Consolas;}}\\fs16\n'
        sed -e 's/\\/\\\\/g' -e 's/{/\\{/g' -e 's/}/\\}/g' -e 's/$/\\par/' "$root/LICENSE"
        printf '}\n'
    } > "$work/license.rtf"
    local msi=$dist/Tratto-x64.msi
    (cd "$tools" && ./wix.exe build -arch x64 -nologo \
        -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext \
        -d "Version=$version" -d "Stage=$(cygpath -w "$stage")" \
        -d "LicenseRtf=$(cygpath -w "$work/license.rtf")" \
        -d "Icon=$(cygpath -w "$root/crates/ui/assets/icon/icon.ico")" \
        -o "$(cygpath -w "$msi")" "$(cygpath -w "$root/packaging/windows/tratto.wxs")")
    rm -f "${msi%.msi}.wixpdb"
    sign_windows "$msi"
    echo "    dist/Tratto-x64.msi"
}

# The AppImage, from the portable app in $1. It uses the system's graphics,
# Wayland and X11 libraries, as the portable app does.
linux_appimage() {
    local stage=$1 work=$2
    local appdir=$work/Tratto.AppDir
    mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" "$appdir/usr/share/doc/tratto" \
        "$appdir/usr/share/icons/hicolor/256x256/apps"
    cp "$stage/tratto" "$stage/libpdfium.so" "$appdir/usr/bin/"
    cp -r "$stage/LICENSE" "$stage/licenses" "$appdir/usr/share/doc/tratto/"
    cp "$root/packaging/linux/tratto.desktop" "$appdir/"
    cp "$root/packaging/linux/tratto.desktop" "$appdir/usr/share/applications/"
    cp "$root/crates/ui/assets/icon/icon.png" "$appdir/tratto.png"
    cp "$root/crates/ui/assets/icon/icon.png" "$appdir/usr/share/icons/hicolor/256x256/apps/tratto.png"
    ln -s usr/bin/tratto "$appdir/AppRun"

    local tool=${XDG_CACHE_HOME:-$HOME/.cache}/tratto/appimagetool-$appimagetool_version-x86_64.AppImage
    if [ ! -x "$tool" ]; then
        echo "    fetching appimagetool $appimagetool_version"
        mkdir -p "$(dirname "$tool")"
        curl -fsSL -o "$tool.part" \
            "https://github.com/AppImage/appimagetool/releases/download/$appimagetool_version/appimagetool-x86_64.AppImage"
        chmod 755 "$tool.part"
        mv "$tool.part" "$tool"
    fi
    # Unpacked rather than mounted: no FUSE needed (WSL, CI).
    ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$appdir" "$dist/Tratto-x86_64.AppImage" > /dev/null
    echo "    dist/Tratto-x86_64.AppImage"
}

# Tratto.app, from the portable app in $1, zipped for the updater and in
# a disk image to install from.
macos_app() {
    local stage=$1 work=$2 target=$3
    local app=$work/Tratto.app
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
    sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
    cp "$stage/tratto" "$app/Contents/MacOS/"
    cp "$stage/libpdfium.dylib" "$app/Contents/Frameworks/"
    cp "$root/crates/ui/assets/icon/icon.icns" "$app/Contents/Resources/Tratto.icns"
    cp -r "$stage/LICENSE" "$stage/licenses" "$app/Contents/Resources/"

    # Signed inside out: the library, then the bundle sealing it. Without
    # an identity, ad hoc (Apple silicon runs nothing unsigned).
    local identity=${TRATTO_MACOS_IDENTITY:--} options=()
    [ "$identity" = - ] || options=(--options runtime --timestamp)
    codesign --force "${options[@]}" --sign "$identity" "$app/Contents/Frameworks/libpdfium.dylib"
    codesign --force "${options[@]}" --sign "$identity" "$app"

    rm -f "$dist/tratto-$target.app.zip"
    ditto -c -k --norsrc --noextattr --keepParent "$app" "$dist/tratto-$target.app.zip"
    echo "    dist/tratto-$target.app.zip"

    local image=$work/image dmg=$dist/Tratto-arm64.dmg
    mkdir -p "$image"
    cp -R "$app" "$image/"
    ln -s /Applications "$image/Applications"
    rm -f "$dmg"
    hdiutil create -quiet -volname Tratto -srcfolder "$image" -fs HFS+ -format UDZO "$dmg"
    if [ "$identity" != - ]; then
        codesign --force --timestamp --sign "$identity" "$dmg"
        if [ -n "${APPLE_ID:-}" ]; then
            echo "    notarizing"
            xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
                --password "$APPLE_APP_PASSWORD" --wait
            xcrun stapler staple "$dmg"
        fi
    fi
    echo "    dist/Tratto-arm64.dmg"
}

build() {
    [ "$steps" = bundle ] || compile "$1"
    [ "$steps" = compile ] || bundle "$1"
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
    wsl.exe -- bash -lc "cd '$wsl_root' && CARGO_TARGET_DIR=\$HOME/.cache/tratto/target bash scripts/package.sh $steps_flag linux"
}

steps=all steps_flag=
case ${1:-} in
    --build-only) steps=compile steps_flag=$1; shift ;;
    --bundle-only) steps=bundle steps_flag=$1; shift ;;
esac

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
