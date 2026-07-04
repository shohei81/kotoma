# kotoma install / update for Windows.
# Usage (from PowerShell):
#   irm https://raw.githubusercontent.com/shohei81/kotoma/main/install.ps1 | iex
# With a model preset or source build:
#   & ([scriptblock]::Create((irm .../install.ps1))) -Tier high
#   & ([scriptblock]::Create((irm .../install.ps1))) -FromSource
param(
    [string]$Tier = "",
    [switch]$FromSource
)
$ErrorActionPreference = "Stop"

$RepoUrl = "https://github.com/shohei81/kotoma"
$Asset = "kotoma-x86_64-pc-windows-msvc.zip"
$BinDir = Join-Path $env:LOCALAPPDATA "kotoma\bin"

if ($Tier -and $Tier -notin @("standard", "high", "both")) {
    Write-Error "unknown tier '$Tier' (expected: standard | high | both)"
}

Write-Host "==> Installing / updating kotoma"
if ($FromSource) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Error "cargo not found - install Rust first: https://rustup.rs (also needs CMake + MSVC build tools)"
    }
    cargo install --git $RepoUrl --force kotoma
    $Bin = "kotoma"
} else {
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    $zip = Join-Path $env:TEMP "kotoma.zip"
    Invoke-WebRequest -Uri "$RepoUrl/releases/latest/download/$Asset" -OutFile $zip
    Expand-Archive -Path $zip -DestinationPath $BinDir -Force
    Remove-Item $zip
    $Bin = Join-Path $BinDir "kotoma.exe"
    Write-Host "  installed -> $Bin"

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -notlike "*$BinDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$userPath;$BinDir", "User")
        Write-Host "  added $BinDir to your user PATH (restart the terminal to pick it up)"
    }
}

if (-not (Get-Command llama-server -ErrorAction SilentlyContinue)) {
    Write-Host "NOTE: llama-server not found on PATH - translation will be disabled."
    Write-Host "      Install llama.cpp: grab a release zip from"
    Write-Host "      https://github.com/ggml-org/llama.cpp/releases and add it to PATH."
}

if ($Tier) {
    Write-Host "==> Installing the '$Tier' model preset"
    & $Bin model preset $Tier
}

Write-Host ""
Write-Host "Run: kotoma notes.md"
