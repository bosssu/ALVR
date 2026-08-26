@echo off
setlocal
cd /d "%~dp0"

REM One-click build for alvr_client_mock (fake ALVR client, no headset).
REM Output: target\release\alvr_client_mock.exe
REM Extra args are forwarded to cargo, e.g. --offline

where cargo >nul 2>&1
if errorlevel 1 (
  echo cargo not found. Install Rust and reopen this window.
  echo https://rustup.rs/
  pause
  exit /b 1
)

echo Building alvr_client_mock (release)...
cargo build -p alvr_client_mock --release %*
set EXITCODE=%ERRORLEVEL%

if not "%EXITCODE%"=="0" (
  echo.
  echo Mock client build failed with exit code %EXITCODE%.
  echo Expected: "%~dp0target\release\alvr_client_mock.exe"
  pause
  exit /b %EXITCODE%
)

if not exist "%~dp0build\alvr_client_mock" mkdir "%~dp0build\alvr_client_mock"
copy /Y "%~dp0target\release\alvr_client_mock.exe" "%~dp0build\alvr_client_mock\alvr_client_mock.exe" >nul
if errorlevel 1 (
  echo.
  echo Build OK but copy to build\alvr_client_mock failed.
  echo Binary: "%~dp0target\release\alvr_client_mock.exe"
  pause
  exit /b 1
)

echo.
echo Mock client build OK.
echo Output: "%~dp0build\alvr_client_mock\alvr_client_mock.exe"
echo Also:   "%~dp0target\release\alvr_client_mock.exe"
echo.
echo Run after ALVR Dashboard + SteamVR. Trust the client, uncheck random orientation.
pause
exit /b 0
