@echo off
setlocal
cd /d "%~dp0"

REM Build Windows streamer only (Dashboard + SteamVR driver).
REM Output: build\alvr_streamer_windows\
REM Extra args are forwarded to build-all.ps1, e.g. -SkipDeps -NoGpl -DebugBuild

powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-all.ps1" -Target Streamer %*
set EXITCODE=%ERRORLEVEL%

if not "%EXITCODE%"=="0" (
  echo.
  echo Streamer build failed with exit code %EXITCODE%.
  echo Output dir: "%~dp0build\alvr_streamer_windows"
  pause
  exit /b %EXITCODE%
)

echo.
echo Streamer build OK.
echo Output: "%~dp0build\alvr_streamer_windows"
pause
exit /b 0
