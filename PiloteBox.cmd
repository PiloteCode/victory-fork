@echo off
rem PiloteBox: double-click to play.
rem It builds the release program when the sources changed (cargo build
rem --release -p victory), then runs "pilotebox play" with the arguments
rem given to this file, for instance:
rem     PiloteBox.cmd --mode s        the Series S profile
rem     PiloteBox.cmd --rt            the ray-tracing structure builder on
rem     PiloteBox.cmd --safe          the play defaults without the newest ways
rem     PiloteBox.cmd --game D:\games\title\title.exe
rem When the program ends with an error, the window stays open and says
rem where the crash log is.
setlocal
cd /d "%~dp0"
set "PB=%~dp0target\release\pilotebox.exe"

where cargo >nul 2>nul
if errorlevel 1 (
    if not exist "%PB%" (
        echo Rust is not installed and %PB% is not built: see README.md, Quick start.
        pause
        exit /b 1
    )
) else (
    cargo build --release -p victory
    if errorlevel 1 (
        echo.
        echo The build failed: see the lines above.
        pause
        exit /b 1
    )
)

"%PB%" play %*
set "CODE=%ERRORLEVEL%"
if "%CODE%"=="0" exit /b 0

echo.
rem One line: this text, then what "play --crash-log" says of the crash log.
<nul set /p "=PiloteBox ended with an error (code %CODE%). "
"%PB%" play --crash-log %* 2>nul || echo See the lines above.
pause
exit /b %CODE%
