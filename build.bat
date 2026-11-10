@echo off
rem Builds the Rust version and copies KbBatteryTray.exe next to this file (autostart points here).
cd /d "%~dp0"
cargo build --release || exit /b 1
taskkill /im KbBatteryTray.exe /f >nul 2>&1
copy /y target\release\KbBatteryTray.exe KbBatteryTray.exe >nul
