@echo off
REM whisper-dictate - compatibility launcher (Windows).
REM The installed Start-menu shortcut launches the Rust UI directly.
REM This compatibility launcher stays terminal-oriented so debug runs keep
REM a visible console even though the Rust UI is a Windows GUI app.
setlocal
set "RUST_EXE=%~dp0whisper-dictate.exe"

if exist "%RUST_EXE%" (
  if /I "%~1"=="--settings-ui" (
    "%RUST_EXE%" ui
  ) else (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
  )
  set "rc=%ERRORLEVEL%"
  echo.
  pause
  exit /b %rc%
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
set "rc=%ERRORLEVEL%"
echo.
pause
exit /b %rc%
