# Builds Set Creator.exe:
#   1. the editor UI from the GameShow frontend (bun run build:desktop) into ui/
#   2. the Tauri app (cargo build --release), which packs ui/ into the exe
# Needs (on the build PC only): Bun, Rust, and the GameShow repo. Set GAMESHOW_DIR if it isn't
# at D:\GameShow. The result is one portable exe: SetCreator.exe next to this script.
# Native tools print progress on stderr; failures are caught by exit code instead.
$ErrorActionPreference = "Continue"
$app = $PSScriptRoot
$gameshow = if ($env:GAMESHOW_DIR) { $env:GAMESHOW_DIR } else { "D:\GameShow" }
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

Write-Host "==> Building the editor UI from $gameshow\frontend"
$env:SET_CREATOR_OUT = Join-Path $app "ui"
Push-Location (Join-Path $gameshow "frontend")
try {
    bun run build:desktop
    if ($LASTEXITCODE -ne 0) { throw "UI build failed" }
} finally {
    Pop-Location
    Remove-Item Env:\SET_CREATOR_OUT
}

Write-Host "==> Building the exe"
Push-Location $app
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

Copy-Item (Join-Path $app "target\release\set-creator.exe") (Join-Path $app "SetCreator.exe") -Force
$size = [math]::Round((Get-Item (Join-Path $app "SetCreator.exe")).Length / 1MB, 1)
Write-Host "==> Done: $app\SetCreator.exe ($size MB)"
