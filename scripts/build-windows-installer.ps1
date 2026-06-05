<#
.SYNOPSIS
    Build the crowd-cast Windows installer (release binary + Inno Setup package).

.DESCRIPTION
    Compiles the release agent binary and runs the Inno Setup compiler (ISCC) on
    installer\windows\crowd-cast.iss to produce dist\crowd-cast-setup-<version>.exe.

    The upload endpoint is baked in at build time, so CROWD_CAST_API_GATEWAY_URL
    must be set (env var) or passed via -ApiGatewayUrl.

.EXAMPLE
    $env:CROWD_CAST_API_GATEWAY_URL = "https://.../prod/presign"
    pwsh scripts\build-windows-installer.ps1

.EXAMPLE
    pwsh scripts\build-windows-installer.ps1 -ApiGatewayUrl "https://.../prod/presign" -Version 1.0.3
#>
[CmdletBinding()]
param(
    [string]$ApiGatewayUrl = $env:CROWD_CAST_API_GATEWAY_URL,
    [string]$Version,
    [string]$Iscc
)

$ErrorActionPreference = 'Stop'
$repoRoot   = Split-Path -Parent $PSScriptRoot
$iss        = Join-Path $repoRoot 'installer\windows\crowd-cast.iss'
$releaseDir = Join-Path $repoRoot 'target\release'
$exePath    = Join-Path $releaseDir 'crowd-cast-agent.exe'
$obsDll     = Join-Path $releaseDir 'obs.dll'

if ([string]::IsNullOrWhiteSpace($ApiGatewayUrl)) {
    throw "CROWD_CAST_API_GATEWAY_URL is required (set the env var or pass -ApiGatewayUrl)."
}

# Version: default to the [package] version in Cargo.toml.
if ([string]::IsNullOrWhiteSpace($Version)) {
    $line = Select-String -Path (Join-Path $repoRoot 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $line) { throw "Could not read version from Cargo.toml." }
    $Version = $line.Matches[0].Groups[1].Value
}
# Inno's VersionInfoVersion needs a numeric x.y.z.b; strip any pre-release suffix.
$numeric = ($Version -split '[-+]')[0]
$parts = $numeric.Split('.')
while ($parts.Count -lt 4) { $parts += '0' }
$versionInfo = ($parts[0..3]) -join '.'

# Locate ISCC.
if (-not $Iscc) {
    $candidates = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    )
    $Iscc = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $Iscc) { $Iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source }
}
if (-not $Iscc -or -not (Test-Path $Iscc)) {
    throw "ISCC.exe (Inno Setup 6) not found. Install it: winget install JRSoftware.InnoSetup"
}

Write-Host "==> Building release binary (v$Version)..." -ForegroundColor Cyan
$env:CROWD_CAST_API_GATEWAY_URL = $ApiGatewayUrl
& cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed." }
if (-not (Test-Path $exePath)) { throw "Expected binary not found at $exePath." }
# obs.dll is the loader the agent links against; it must ship so the process can
# start (the rest of the OBS runtime is downloaded on first launch).
if (-not (Test-Path $obsDll)) { throw "obs.dll not found at $obsDll (expected from the libobs-rs build)." }

Write-Host "==> Compiling installer (ISCC)..." -ForegroundColor Cyan
& $Iscc "/DAppVersion=$Version" "/DAppVersionInfo=$versionInfo" "/DSourceDir=$releaseDir" $iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed." }

$out = Join-Path $repoRoot "dist\crowd-cast-setup-$Version.exe"
if (Test-Path $out) {
    Write-Host "==> Installer built: $out" -ForegroundColor Green
} else {
    Write-Warning "ISCC reported success but $out was not found; check dist\."
}
