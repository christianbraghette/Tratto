# SPDX-FileCopyrightText: 2026 Christian Braghette
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# package.sh for Windows without Git Bash: builds the same release files,
# `dist/Tratto-x64.msi` (the installer, made with WiX, fetched; needs the
# .NET runtime 6 or later) and `dist/tratto-<target>.zip` (tratto.exe, the
# PDFium library, LICENSE and licenses/), from PowerShell (Windows
# PowerShell 5.1 or PowerShell 7).
#
# With TRATTO_SIGN_CERT set to the SHA-1 thumbprint of a code signing
# certificate in the user's store, tratto.exe and the installer are signed.
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
$wixVersion = '5.0.2'
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.*)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
# Windows' own tar (bsdtar) reads .tgz and .zip and writes zips.
$tar = Join-Path $env:SystemRoot 'System32\tar.exe'

function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments)
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Exe failed (exit code $LASTEXITCODE)" }
}

# Signs $Path with the certificate TRATTO_SIGN_CERT, if set.
function Invoke-Sign {
    param([string]$Path)
    if (-not $env:TRATTO_SIGN_CERT) { return }
    $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
        Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
    if (-not $signtool) { throw 'signtool not found: install the Windows SDK' }
    Invoke-Native $signtool.FullName @('sign', '/sha1', $env:TRATTO_SIGN_CERT, '/fd', 'sha256', '/td', 'sha256',
        '/tr', 'http://timestamp.digicert.com', '/d', 'Tratto', $Path)
}

# The installer, from the portable app in $Stage (see packaging\windows\tratto.wxs).
function Build-Msi {
    param([string]$Stage, [string]$TargetDir)
    # WiX (its NuGet package, run on whichever .NET runtime is installed)
    # and its extensions go to a folder of their own; `wix` finds the
    # extensions from there.
    $tools = Join-Path $TargetDir "tools\wix-$wixVersion"
    $wix = Join-Path $tools 'wix.exe'
    $env:DOTNET_ROLL_FORWARD = 'Major'
    if (-not (Test-Path $wix)) {
        Write-Host "    fetching WiX $wixVersion"
        $part = "$tools.part"
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $part
        New-Item -ItemType Directory -Force $part | Out-Null
        Invoke-Native 'curl.exe' @('-fsSL', '-o', "$part\wix.nupkg", "https://www.nuget.org/api/v2/package/wix/$wixVersion")
        Invoke-Native $tar @('-xf', "$part\wix.nupkg", '-C', $part, 'tools/net6.0/any')
        Move-Item (Join-Path $part 'tools\net6.0\any') $tools
        Remove-Item -Recurse -Force $part
        Push-Location $tools
        try {
            Invoke-Native $wix @('extension', 'add', "WixToolset.UI.wixext/$wixVersion", "WixToolset.Util.wixext/$wixVersion")
        } finally {
            Pop-Location
        }
    }
    # The license page: LICENSE as RTF.
    $rtf = Join-Path (Split-Path -Parent $Stage) 'license.rtf'
    $lines = Get-Content (Join-Path $root 'LICENSE') |
        ForEach-Object { ($_.Replace('\', '\\').Replace('{', '\{').Replace('}', '\}')) + '\par' }
    Set-Content -Encoding Ascii $rtf (@('{\rtf1\ansi\deff0{\fonttbl{\f0 Consolas;}}\fs16') + $lines + '}')

    $msi = Join-Path $dist 'Tratto-x64.msi'
    Push-Location $tools
    try {
        Invoke-Native $wix @('build', '-arch', 'x64', '-nologo',
            '-ext', 'WixToolset.UI.wixext', '-ext', 'WixToolset.Util.wixext',
            '-d', "Version=$version", '-d', "Stage=$Stage", '-d', "LicenseRtf=$rtf",
            '-d', "Icon=$(Join-Path $root 'crates\ui\assets\icon\icon.ico')",
            '-o', $msi, (Join-Path $root 'packaging\windows\tratto.wxs'))
    } finally {
        Pop-Location
    }
    Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($msi, 'wixpdb'))
    Invoke-Sign $msi
    Write-Host '    dist\Tratto-x64.msi'
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
    # The portable app, unpacked: what the zip holds and the installer is
    # made from.
    $work = Join-Path ([IO.Path]::GetTempPath()) "tratto-package-$PID"
    $stage = Join-Path $work 'stage'
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $work
    New-Item -ItemType Directory -Force (Join-Path $stage 'licenses'), $dist | Out-Null
    try {
        Copy-Item (Join-Path $targetDir "$target\release\tratto.exe") $stage
        Copy-Item $lib $stage
        Copy-Item (Join-Path $root 'LICENSE') $stage
        Copy-Item (Join-Path $pdfiumDir 'LICENSE') (Join-Path $stage 'licenses\PDFium-LICENSE')
        Copy-Item (Join-Path $pdfiumDir 'licenses\*') (Join-Path $stage 'licenses')
        Invoke-Sign (Join-Path $stage 'tratto.exe')

        $zip = Join-Path $dist "tratto-$target.zip"
        Remove-Item -Force -ErrorAction SilentlyContinue $zip
        Push-Location $stage
        try {
            Invoke-Native $tar (@('-a', '-cf', $zip) + (Get-ChildItem -Name))
        } finally {
            Pop-Location
        }
        Write-Host "    dist\tratto-$target.zip"
        Build-Msi $stage $targetDir
    } finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $work
    }
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
