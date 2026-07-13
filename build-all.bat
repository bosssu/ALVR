@echo off
setlocal
cd /d "%~dp0"

REM Double-click friendly wrapper for build-all.ps1
REM Examples:
REM   build-all.bat
REM   build-all.bat -Target Streamer
REM   build-all.bat -Target Client -SkipDeps
REM   build-all.bat -Target All -ForceDeps

powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-all.ps1" %*
set EXITCODE=%ERRORLEVEL%

if not "%EXITCODE%"=="0" (
  echo.
  echo Build failed with exit code %EXITCODE%.
  pause
  exit /b %EXITCODE%
)

echo.
pause
exit /b 0
