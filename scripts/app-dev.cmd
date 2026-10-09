@echo off
rem The template for an app's dev launcher, scripts\<app>-dev.cmd in its repo
rem (docs/STANDARD.md, "The dev launcher"): copy it, replace <app> and the run
rem line, then make its Start Menu shortcut with
rem   powershell -File <v15-kit>\scripts\dev-shortcut.ps1 -Launcher scripts\<app>-dev.cmd
rem
rem <app>-dev: the app's debug build, run from this repo. It installs nothing,
rem never updates and keeps its own data (<identifier>-dev), so it runs beside
rem the installed <app>. The Start Menu's "<app>-dev" shortcut runs this.
cd /d "%~dp0.."
title <app>-dev
rem An app with a dev server starts it first, minimized, unless something
rem already listens on its port (star-dev's shape):
rem powershell -NoProfile -Command "if (-not (Get-NetTCPConnection -LocalPort 8080 -State Listen -ErrorAction SilentlyContinue)) { Start-Process node -ArgumentList 'server.js' -WindowStyle Minimized }"
cargo run -p <app>-app
rem keep a failed build's errors on screen
if errorlevel 1 pause
