@echo off
setlocal
title Horama (Rust + Slint)

cd /d "%~dp0"

set "MINGW_BIN=%LOCALAPPDATA%\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin"
if exist "%MINGW_BIN%" set "PATH=%MINGW_BIN%;%PATH%"
if exist "%USERPROFILE%\.cargo\bin" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

if exist "target\release\horama.exe" goto :run_exe

echo Building Horama (Rust + Slint) in Release mode...
cargo build --release
if errorlevel 1 goto :build_error

:run_exe
start "" "target\release\horama.exe"
exit /b 0

:build_error
echo.
echo Build failed with error code %ERRORLEVEL%.
pause
exit /b 1
