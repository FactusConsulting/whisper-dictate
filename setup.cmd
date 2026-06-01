@echo off
REM whisper-dictate - compatibility launcher (Windows).
REM Prefer the Rust controller when bundled, with setup.ps1 retained as
REM the fallback for older portable folders.
setlocal
set "RUST_EXE=%~dp0whisper-dictate.exe"

if exist "%RUST_EXE%" (
  if /I "%~1"=="--settings-ui" (
    "%RUST_EXE%" ui
  ) else if /I "%~1"=="--doctor" (
    "%RUST_EXE%" doctor
  ) else (
    "%RUST_EXE%" run %*
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
