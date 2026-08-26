@echo off
setlocal
cd /d "%~dp0"

REM Build Android client APK only.
REM Output: build\alvr_client_android\alvr_client_android.apk
REM Extra args are forwarded to build-all.ps1, e.g. -SkipDeps -DebugBuild

powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-all.ps1" -Target Client %*
set EXITCODE=%ERRORLEVEL%

if not "%EXITCODE%"=="0" (
  echo.
  echo Client build failed with exit code %EXITCODE%.
  echo Output dir: "%~dp0build\alvr_client_android"
  pause
  exit /b %EXITCODE%
)

echo.
echo Client build OK.
echo Output: "%~dp0build\alvr_client_android\alvr_client_android.apk"
pause
exit /b 0
