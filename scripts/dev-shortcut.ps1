<#
Creates or refreshes the per-user Start Menu shortcut that runs an app's dev
launcher (docs/STANDARD.md, "The dev launcher").

  powershell -NoProfile -File <v15-kit>\scripts\dev-shortcut.ps1 -Launcher <repo>\scripts\<app>-dev.cmd

The shortcut is named after the launcher (<app>-dev), starts in the launcher's
parent folder (the app's root), and wears the app's icon: -Icon, or the first
of src-tauri\icons\icon.ico and icons\icon.ico under that root. Run it again
after moving the repo; -Remove deletes the shortcut.
#>
param(
    [Parameter(Mandatory)][string]$Launcher,
    [string]$Name,
    [string]$Icon,
    [switch]$Remove
)
$ErrorActionPreference = 'Stop'

$Launcher = (Resolve-Path -LiteralPath $Launcher).Path
$root = Split-Path (Split-Path $Launcher)
if (-not $Name) { $Name = [IO.Path]::GetFileNameWithoutExtension($Launcher) }
$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) "$Name.lnk"

if ($Remove) {
    Remove-Item -LiteralPath $lnk -Force -ErrorAction SilentlyContinue
    "removed $lnk"
    return
}

if (-not $Icon) {
    $Icon = 'src-tauri\icons\icon.ico', 'icons\icon.ico' |
        ForEach-Object { Join-Path $root $_ } |
        Where-Object { Test-Path -LiteralPath $_ } |
        Select-Object -First 1
}

$s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
$s.TargetPath = $Launcher
$s.Arguments = ''
$s.WorkingDirectory = $root
$s.Description = "$Name`: the dev build, from $root"
$s.IconLocation = if ($Icon) { "$((Resolve-Path -LiteralPath $Icon).Path),0" } else { '' }
$s.Save()
"$lnk -> $Launcher"
