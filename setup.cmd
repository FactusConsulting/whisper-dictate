@echo off
REM whisper-dictate - double-clickable one-click setup/launcher (Windows).
REM Just double-click this file. It runs setup.ps1 with the execution
REM policy bypassed (a .ps1 can't be double-clicked directly), passing
REM any args straight through. The window stays open at the end so you
REM can read the result / any error.
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
echo.
pause
