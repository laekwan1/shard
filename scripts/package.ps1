<#
.SYNOPSIS
    Assemble standalone distribution folders for Shard and Veil.

.DESCRIPTION
    The two programs share no code at run time and have disjoint dependencies,
    so each ships as its own self-contained folder. Nothing outside the folder
    is needed, and neither folder references the other.

    Cargo stages every vendor file into a single target directory so that
    `cargo run` works; this script is what separates them for distribution.

.PARAMETER SkipBuild
    Package whatever is already in target/release. Useful when the executables
    are running and cannot be relinked.
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$release = Join-Path $root 'target\release'
$vendor = Join-Path $root 'vendor'
# Everything shippable lives under `release`, split by platform and then by
# app: release\PC\Shard, release\android\Veil, and so on. One tree rather than
# two means there is never a question of which folder is the current one.
$dist = Join-Path $root 'release\PC'

if (-not $SkipBuild) {
    Write-Host '==> building release' -ForegroundColor Cyan
    Push-Location $root
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) {
            throw 'cargo build failed. If the apps are running, quit them from the tray or pass -SkipBuild.'
        }
    } finally {
        Pop-Location
    }
}

# Each entry is source -> path relative to that program's folder.
$layouts = @{
    'Shard' = @(
        @{ From = "$release\shard.exe";            To = 'shard.exe' }
        @{ From = "$vendor\windivert\WinDivert.dll";   To = 'WinDivert.dll' }
        @{ From = "$vendor\windivert\WinDivert64.sys"; To = 'WinDivert64.sys' }
    )
    'Veil' = @(
        @{ From = "$release\veil.exe";             To = 'veil.exe' }
        @{ From = "$vendor\singbox\sing-box.exe";  To = 'sing-box.exe' }
        @{ From = "$vendor\singbox\wintun.dll";    To = 'wintun.dll' }
        @{ From = "$vendor\tor\tor.exe";           To = 'tor\tor.exe' }
        @{ From = "$vendor\tor\pt_config.json";    To = 'tor\pt_config.json' }
        @{ From = "$vendor\tor\pluggable_transports\lyrebird.exe"; To = 'tor\pluggable_transports\lyrebird.exe' }
        @{ From = "$vendor\tor\data\geoip";        To = 'tor\data\geoip' }
        @{ From = "$vendor\tor\data\geoip6";       To = 'tor\data\geoip6' }
    )
}

foreach ($name in 'Shard', 'Veil') {
    $target = Join-Path $dist $name
    Write-Host "==> packaging $name" -ForegroundColor Cyan

    # Overwrite in place rather than clearing the folder first. A running app
    # holds its own files open — WinDivert64.sys in particular is locked by the
    # driver service — and a recursive delete would leave a half-emptied
    # distribution behind when it hit the first locked file.
    New-Item -ItemType Directory -Force $target | Out-Null

    $locked = @()
    foreach ($item in $layouts[$name]) {
        if (-not (Test-Path $item.From)) { throw "missing: $($item.From)" }
        $destination = Join-Path $target $item.To
        $parent = Split-Path -Parent $destination
        if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Force $parent | Out-Null }
        # Vendored binaries never change between builds, and a running app can
        # hold them open. Skipping an identical file avoids a pointless warning.
        $existing = Get-Item $destination -ErrorAction SilentlyContinue
        $source = Get-Item $item.From
        if ($existing -and $existing.Length -eq $source.Length -and $existing.LastWriteTime -ge $source.LastWriteTime) {
            continue
        }
        try {
            Copy-Item $item.From $destination -Force -ErrorAction Stop
        } catch {
            $locked += $item.To
        }
    }
    if ($locked.Count -gt 0) {
        Write-Host "    in use, not replaced: $($locked -join ', ')" -ForegroundColor Yellow
        Write-Host "    quit $name from the tray and rerun to update these." -ForegroundColor Yellow
    }

    $readme = Join-Path $root "docs\$name.md"
    if (Test-Path $readme) { Copy-Item $readme (Join-Path $target 'README.md') -Force }

    $bytes = (Get-ChildItem $target -Recurse -File | Measure-Object -Property Length -Sum).Sum
    Write-Host ("    {0,-6} {1,7:N1} MB" -f $name, ($bytes / 1MB)) -ForegroundColor Green
}

Write-Host ''
Write-Host "PC folders written to $dist" -ForegroundColor Cyan
Write-Host 'each folder is self-contained; move or zip it whole.'
