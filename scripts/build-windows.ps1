<#
.SYNOPSIS
Builds Film Scanner as a standalone Windows program, dist\Film Scanner.exe.

.DESCRIPTION
The .exe is self-contained: copy it anywhere and double-click it. With -StartMenu or
-Desktop it also adds a shortcut there. Epson's firmware isn't bundled; native V500 scans
load it from the Epson Scan install, as when running from source.

.EXAMPLE
.\scripts\build-windows.ps1 -StartMenu -Desktop
#>
param(
    [switch]$StartMenu,
    [switch]$Desktop
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

cargo build --release -p film-scanner --manifest-path "$root\Cargo.toml"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
$exe = Join-Path $dist 'Film Scanner.exe'
Copy-Item (Join-Path $root 'target\release\film-scanner.exe') $exe -Force
Write-Host "Built $exe"

function New-Shortcut([string]$folder) {
    $link = Join-Path $folder 'Film Scanner.lnk'
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($link)
    $shortcut.TargetPath = $exe
    $shortcut.WorkingDirectory = $dist
    $shortcut.IconLocation = "$exe,0"
    $shortcut.Description = 'Film Scanner'
    $shortcut.Save()
    Write-Host "Shortcut $link"
}
if ($StartMenu) { New-Shortcut ([Environment]::GetFolderPath('Programs')) }
if ($Desktop) { New-Shortcut ([Environment]::GetFolderPath('Desktop')) }
