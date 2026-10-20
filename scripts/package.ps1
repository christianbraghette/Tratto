# SPDX-FileCopyrightText: 2026 Christian Braghette
# SPDX-License-Identifier: GPL-3.0-or-later
#
# package.sh for Windows without Git Bash: builds the same release archives,
# `dist/tratto-<target>.zip` (tratto.exe, the PDFium library, LICENSE and
# licenses/), from PowerShell (Windows PowerShell 5.1 or PowerShell 7).
#
# Usage: scripts\package.ps1 [windows] [linux] [macos]    (none: all three)
#
# * windows  is built here.
# * linux    is built in WSL, by package.sh (run there once:
#              sudo apt install build-essential pkg-config libwayland-dev libxkbcommon-dev
#              curl https://sh.rustup.rs -sSf | sh -s -- -y
#            ).
# * macos    on a Mac only: build it on GitHub instead: Actions > Release >
#            Run workflow (the archives are in the run's artifacts).
#
# PDFium is fetched into crates\ui\pdfium\ when missing.
#
# If scripts are blocked: powershell -ExecutionPolicy Bypass -File scripts\package.ps1

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist'
# See crates/ui/pdfium/README.md.
$pdfiumRelease = 'chromium%2F7881'
# Windows' own tar (bsdtar) reads .tgz and writes zips.
$tar = Join-Path $env:SystemRoot 'System32\tar.exe'

function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments)
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Exe failed (exit code $LASTEXITCODE)" }
}

function Build-Windows {
    $target = 'x86_64-pc-windows-msvc'
    Write-Host "==> windows ($target)"

    $pdfiumDir = Join-Path $root 'crates\ui\pdfium\win-x64'
    $lib = Join-Path $pdfiumDir 'bin\pdfium.dll'
    if (-not (Test-Path $lib)) {
        Write-Host '    fetching PDFium (win-x64)'
        New-Item -ItemType Directory -Force $pdfiumDir | Out-Null
        $tgz = Join-Path ([IO.Path]::GetTempPath()) "pdfium-win-x64-$PID.tgz"
        try {
            Invoke-Native 'curl.exe' @('-fsSL', '-o', $tgz,
                "https://github.com/bblanchon/pdfium-binaries/releases/download/$pdfiumRelease/pdfium-win-x64.tgz")
            Invoke-Native $tar @('-xzf', $tgz, '-C', $pdfiumDir)
        } finally {
            Remove-Item -Force -ErrorAction SilentlyContinue $tgz
        }
    }

    if (Get-Command rustup -ErrorAction SilentlyContinue) {
        Invoke-Native 'rustup' @('target', 'add', $target) | Out-Null
    }
    Push-Location $root
    try {
        Invoke-Native 'cargo' @('build', '--release', '--locked', '--target', $target)
    } finally {
        Pop-Location
    }

    $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
    $stage = Join-Path ([IO.Path]::GetTempPath()) "tratto-package-$PID"
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $stage
    New-Item -ItemType Directory -Force (Join-Path $stage 'licenses'), $dist | Out-Null
    try {
        Copy-Item (Join-Path $targetDir "$target\release\tratto.exe") $stage
        Copy-Item $lib $stage
        Copy-Item (Join-Path $root 'LICENSE') $stage
        Copy-Item (Join-Path $pdfiumDir 'LICENSE') (Join-Path $stage 'licenses\PDFium-LICENSE')
        Copy-Item (Join-Path $pdfiumDir 'licenses\*') (Join-Path $stage 'licenses')

        $zip = Join-Path $dist "tratto-$target.zip"
        Remove-Item -Force -ErrorAction SilentlyContinue $zip
        Push-Location $stage
        try {
            Invoke-Native $tar (@('-a', '-cf', $zip) + (Get-ChildItem -Name))
        } finally {
            Pop-Location
        }
    } finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $stage
    }
    Write-Host "    dist\tratto-$target.zip"
}

# The Linux build runs package.sh in WSL, with its own target folder (faster
# than building on the Windows drive, and apart from the Windows build).
function Build-LinuxInWsl {
    if (-not (Get-Command wsl.exe -ErrorAction SilentlyContinue)) {
        Write-Host '==> linux: skipped (needs WSL)'
        return $false
    }
    $wslRoot = (& wsl.exe -e wslpath -a $root | Out-String).Trim()
    & wsl.exe -- bash -lc 'command -v cargo > /dev/null && pkg-config --exists wayland-client' | Out-Host
    if ($LASTEXITCODE -ne 0) {
        Write-Host '==> linux: skipped, WSL is not set up for building; run once in WSL:'
        Write-Host '      sudo apt install build-essential pkg-config libwayland-dev libxkbcommon-dev'
        Write-Host '      curl https://sh.rustup.rs -sSf | sh -s -- -y'
        return $false
    }
    & wsl.exe -- bash -lc "cd '$wslRoot' && CARGO_TARGET_DIR=`$HOME/.cache/tratto/target bash scripts/package.sh linux" | Out-Host
    return $LASTEXITCODE -eq 0
}

# Platforms asked for by name must be built; with none, the others are
# only reported.
$platforms = if ($args.Count -gt 0) { $args } else { @('windows', 'linux', 'macos') }
$strict = $args.Count -gt 0

$skipped = $false
foreach ($platform in $platforms) {
    switch ($platform) {
        'windows' { Build-Windows }
        'linux' { if (-not (Build-LinuxInWsl)) { $skipped = $true } }
        'macos' {
            Write-Host '==> macos: skipped (build it on a Mac, or on GitHub: Actions > Release > Run workflow)'
            $skipped = $true
        }
        default {
            Write-Error "unknown platform: $platform (windows, linux or macos)" -ErrorAction Continue
            exit 2
        }
    }
}
if ($strict -and $skipped) { exit 1 }
