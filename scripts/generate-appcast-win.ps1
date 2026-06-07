<#
.SYNOPSIS
    Generate the WinSparkle appcast (appcast-win.xml) for a Windows release.

.DESCRIPTION
    Ed25519-signs the installer with winsparkle-tool and writes a single-item
    Sparkle/WinSparkle appcast pointing at the given download URL. WinSparkle on
    the client compares <sparkle:version> against the running app's version and
    verifies <sparkle:edSignature> against the embedded public key before
    installing.

.EXAMPLE
    pwsh scripts/generate-appcast-win.ps1 `
        -InstallerPath dist/crowd-cast-setup-1.0.4.exe `
        -Version 1.0.4 `
        -DownloadUrl https://github.com/p-doom/crowd-cast/releases/download/v1.0.4/crowd-cast-setup-1.0.4.exe `
        -PrivateKeyFile $env:TEMP/ed.key `
        -OutFile dist/appcast-win.xml
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InstallerPath,
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$DownloadUrl,
    [Parameter(Mandatory)][string]$PrivateKeyFile,
    # The version WinSparkle COMPARES (e.g. "1.0.4.4217"). Defaults to $Version.
    # $Version itself is only the human-facing shortVersionString.
    [string]$BuildVersion,
    # Passed to the installer by WinSparkle (sparkle:installerArguments). The
    # defaults make the Inno installer apply the update with no UI: /VERYSILENT
    # (no wizard), /SUPPRESSMSGBOXES (no dialogs), /NORESTART (never reboot). The
    # installer relaunches the agent itself (see [Run] Check: WizardSilent).
    [string]$InstallerArguments = '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART',
    [string]$OutFile,
    [string]$Tool
)

$ErrorActionPreference = 'Stop'
if (-not $BuildVersion) { $BuildVersion = $Version }
$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $Tool)    { $Tool = Join-Path $repoRoot 'build\winsparkle\0.9.3\winsparkle-tool.exe' }
if (-not $OutFile) { $OutFile = Join-Path $repoRoot 'dist\appcast-win.xml' }

foreach ($p in @($InstallerPath, $PrivateKeyFile, $Tool)) {
    if (-not (Test-Path $p)) { throw "Not found: $p" }
}

# Ed25519-sign the installer (winsparkle-tool prints the base64 signature).
$signature = (& $Tool sign --private-key-file $PrivateKeyFile $InstallerPath | Select-Object -Last 1).Trim()
if ([string]::IsNullOrWhiteSpace($signature)) { throw "winsparkle-tool produced no signature." }

$length  = (Get-Item $InstallerPath).Length
$pubDate = (Get-Date).ToUniversalTime().ToString('ddd, dd MMM yyyy HH:mm:ss', [System.Globalization.CultureInfo]::InvariantCulture) + ' +0000'

$xml = @"
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>crowd-cast</title>
    <description>crowd-cast agent updates</description>
    <language>en</language>
    <item>
      <title>Version $Version</title>
      <sparkle:version>$BuildVersion</sparkle:version>
      <sparkle:shortVersionString>$Version</sparkle:shortVersionString>
      <pubDate>$pubDate</pubDate>
      <enclosure url="$DownloadUrl"
                 sparkle:version="$BuildVersion"
                 sparkle:edSignature="$signature"
                 sparkle:installerArguments="$InstallerArguments"
                 length="$length"
                 type="application/octet-stream" />
    </item>
  </channel>
</rss>
"@

# Write UTF-8 without BOM (WinSparkle's XML parser dislikes a BOM).
$dir = Split-Path -Parent $OutFile
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
[System.IO.File]::WriteAllText($OutFile, $xml, (New-Object System.Text.UTF8Encoding($false)))

Write-Host "==> Wrote $OutFile (v$Version, $length bytes)" -ForegroundColor Green
Write-Host "    edSignature=$signature"
