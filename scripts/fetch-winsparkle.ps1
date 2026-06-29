<#
.SYNOPSIS
    Fetch the WinSparkle SDK (auto-update library) used by the Windows agent.

.DESCRIPTION
    Downloads the prebuilt WinSparkle release and lays it out under
    build\winsparkle\<version>\ with the x64 DLL/import-lib, the header, and the
    signing tool. Mirrors scripts/fetch-sparkle.sh on macOS. build\ is gitignored,
    so the binaries are not committed.

    The Windows build expects WinSparkle here (or at $env:CROWD_CAST_WINSPARKLE_DIR);
    the installer ships WinSparkle.dll next to the agent, and the release pipeline
    uses winsparkle-tool.exe to generate/sign the Ed25519 update keys.

.EXAMPLE
    pwsh scripts\fetch-winsparkle.ps1
#>
[CmdletBinding()]
param(
    [string]$Version = "0.9.3"
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$destDir  = Join-Path $repoRoot "build\winsparkle\$Version"
$dll      = Join-Path $destDir "WinSparkle.dll"

if (Test-Path $dll) {
    Write-Host "WinSparkle $Version already present at $destDir" -ForegroundColor Green
    return
}

$zipName = "WinSparkle-$Version.zip"
$url     = "https://github.com/vslavik/winsparkle/releases/download/v$Version/$zipName"
$tmp     = Join-Path ([System.IO.Path]::GetTempPath()) "winsparkle-$Version"
New-Item -ItemType Directory -Force -Path $tmp, $destDir | Out-Null
$zip = Join-Path $tmp $zipName

Write-Host "==> Downloading $url" -ForegroundColor Cyan
Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing

$extract = Join-Path $tmp "x"
Expand-Archive -Path $zip -DestinationPath $extract -Force
$root = Join-Path $extract "WinSparkle-$Version"

# Flatten the bits we need into build\winsparkle\<version>\.
Copy-Item (Join-Path $root "x64\Release\WinSparkle.dll") $destDir -Force
Copy-Item (Join-Path $root "x64\Release\WinSparkle.lib") $destDir -Force
Copy-Item (Join-Path $root "include\winsparkle.h")       $destDir -Force
Copy-Item (Join-Path $root "bin\winsparkle-tool.exe")    $destDir -Force

Write-Host "==> WinSparkle $Version ready at $destDir" -ForegroundColor Green
Get-ChildItem $destDir | ForEach-Object { "    $($_.Name)" }
