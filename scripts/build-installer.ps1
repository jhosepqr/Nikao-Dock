# Builds the one-step installer: Muelle_<version>_x64-setup.exe.
# Usage: scripts/build-installer.ps1   (from any folder)
#
# With TAURI_SIGNING_PRIVATE_KEY (and TAURI_SIGNING_PRIVATE_KEY_PASSWORD) set,
# it also writes the updater signature (<installer>.sig). Without them it
# builds an unsigned installer, fine for local testing.

# Not 'Stop': Windows PowerShell turns cargo's progress on stderr into errors;
# each native step checks $LASTEXITCODE instead.
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $target = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
    $triple = 'x86_64-pc-windows-msvc'

    cargo build --release -p muelle
    if ($LASTEXITCODE) { throw 'cargo build muelle failed' }

    # Tauri bundles sidecars from src-tauri/binaries/<name>-<target triple>.exe.
    $binaries = Join-Path $root 'apps/settings/src-tauri/binaries'
    New-Item -ItemType Directory -Force $binaries -ErrorAction Stop | Out-Null
    Copy-Item (Join-Path $target 'release/muelle.exe') (Join-Path $binaries "muelle-$triple.exe") -Force -ErrorAction Stop

    $configs = @('--config', 'src-tauri/tauri.bundle.conf.json')
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
        Write-Warning 'TAURI_SIGNING_PRIVATE_KEY is not set: building without the updater signature.'
        $unsigned = Join-Path $env:TEMP 'muelle-unsigned.conf.json'
        '{ "bundle": { "createUpdaterArtifacts": false } }' | Set-Content -Encoding ascii $unsigned
        $configs += @('--config', $unsigned)
    }
    # The fork workflow sets this. The overlay drops the crafter-station
    # updater endpoint and its public key; the Rust guard is compiled in too.
    if ($env:MUELLE_FORK_BUILD) {
        Write-Warning 'MUELLE_FORK_BUILD: this installer will not update from crafter-station.'
        $configs += @('--config', 'src-tauri/tauri.fork.conf.json')
    }

    Push-Location (Join-Path $root 'apps/settings')
    try {
        pnpm tauri build @configs
        if ($LASTEXITCODE) { throw 'tauri build failed' }
    } finally { Pop-Location }

    Get-ChildItem (Join-Path $target 'release/bundle/nsis/*-setup.exe*') |
        Select-Object FullName, @{ n = 'MB'; e = { [math]::Round($_.Length / 1MB, 2) } }
} finally { Pop-Location }
