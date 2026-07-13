@echo off
setlocal EnableExtensions EnableDelayedExpansion
title ALVR Recording Mux

cd /d "%~dp0"

echo.
echo ========================================
echo   ALVR mux: video + wav -^> mkv
echo   lossless stream copy via ffmpeg
echo   Note: new ALVR builds record live .mkv already.
echo   This script is for older dual-file captures.
echo ========================================
echo.

set "REC_DIR="
if exist "%~dp0Captures\Records" set "REC_DIR=%~dp0Captures\Records"
if not defined REC_DIR if exist "%~dp0build\alvr_streamer_windows\Captures\Records" set "REC_DIR=%~dp0build\alvr_streamer_windows\Captures\Records"
if not defined REC_DIR if exist "%~dp0alvr_streamer_windows\Captures\Records" set "REC_DIR=%~dp0alvr_streamer_windows\Captures\Records"

if not defined REC_DIR (
    echo [ERROR] Captures\Records not found.
    echo Put this script in the streamer root or the ALVR repo root.
    echo Current dir: %CD%
    goto FAIL
)

echo Records dir: !REC_DIR!
echo.

set "FFMPEG="
where ffmpeg >nul 2>&1
if not errorlevel 1 (
    for /f "delims=" %%I in ('where ffmpeg') do (
        if not defined FFMPEG set "FFMPEG=%%I"
    )
)
if not defined FFMPEG if exist "%~dp0deps\windows\ffmpeg\bin\ffmpeg.exe" set "FFMPEG=%~dp0deps\windows\ffmpeg\bin\ffmpeg.exe"
if not defined FFMPEG if exist "%~dp0bin\win64\ffmpeg.exe" set "FFMPEG=%~dp0bin\win64\ffmpeg.exe"
if not defined FFMPEG if exist "%~dp0ffmpeg.exe" set "FFMPEG=%~dp0ffmpeg.exe"

if not defined FFMPEG (
    echo [ERROR] ffmpeg.exe not found.
    echo Install ffmpeg on PATH or use deps\windows\ffmpeg\bin\ffmpeg.exe
    goto FAIL
)

echo Using ffmpeg: !FFMPEG!
echo.

set /a DONE=0
set /a SKIP=0
set /a FAILN=0
set /a FOUND=0

pushd "!REC_DIR!"
if errorlevel 1 goto FAIL

for %%V in (*.h264 *.h265 *.av1 *.hevc) do call :MUX_ONE "%%~fV"

popd

echo ----------------------------------------
echo Videos found: !FOUND!  OK: !DONE!  Skip: !SKIP!  Fail: !FAILN!
echo Output dir: !REC_DIR!
echo ----------------------------------------
echo.

if !FAILN! GTR 0 goto FAIL
if !FOUND! EQU 0 (
    echo No video files found -*.h264 / *.h265 / *.av1-.
    echo Record something in ALVR first.
    goto FAIL
)

echo Done.
pause
exit /b 0

:FAIL
echo.
pause
exit /b 1

REM ---- subroutine: mux one video if matching wav exists ----
:MUX_ONE
set "VIDEO=%~1"
if not exist "!VIDEO!" goto :eof

set /a FOUND+=1
set "STEM=%~n1"
set "WAV=%~dpn1.wav"
set "OUT=%~dpn1.mkv"
set "VNAME=%~nx1"

if not exist "!WAV!" (
    echo [SKIP] no wav for !VNAME!
    set /a SKIP+=1
    goto :eof
)

if exist "!OUT!" (
    echo [SKIP] already exists: !STEM!.mkv
    set /a SKIP+=1
    goto :eof
)

echo [MUX] !VNAME! + !STEM!.wav -^> !STEM!.mkv

"!FFMPEG!" -hide_banner -loglevel warning -stats -fflags +genpts -i "!VIDEO!" -i "!WAV!" -c copy -map 0:v:0 -map 1:a:0 -y "!OUT!"
if errorlevel 1 (
    echo [FAIL] ffmpeg error for !STEM!
    if exist "!OUT!" del /q "!OUT!" >nul 2>&1
    set /a FAILN+=1
) else (
    echo [OK] !STEM!.mkv
    set /a DONE+=1
)
echo.
goto :eof
