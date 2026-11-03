@echo off
rem Runs every check required before merging a branch into main.
rem Usage: scripts\check.bat   (from any folder)
rem The whole output is also written to target\check.log.
rem Exit code: 0 = all green, 1 = a check failed.
setlocal
cd /d "%~dp0.."

if "%~1"=="--run" goto :run

if not exist target mkdir target
rem Show the output live and copy it to the log; keep the exit code of the checks.
rem stderr is merged inside cmd, so PowerShell does not turn cargo's messages into errors.
powershell -NoProfile -ExecutionPolicy Bypass -Command "& cmd /c 'scripts\check.bat --run 2>&1' | Tee-Object -FilePath 'target\check.log'; exit $LASTEXITCODE"
exit /b %errorlevel%

:run
echo Horama check - %date% %time%
for /f "delims=" %%c in ('git rev-parse --abbrev-ref HEAD 2^>nul') do echo Branch: %%c
for /f "delims=" %%c in ('git rev-parse HEAD 2^>nul') do echo Commit: %%c
for /f "delims=" %%c in ('rustc -V') do echo Toolchain: %%c

echo.
echo [1/5] Formatting (cargo fmt --check)
cargo fmt --all -- --check
if errorlevel 1 goto :fail

echo.
echo [2/5] Lints (cargo clippy, warnings are errors)
cargo clippy --locked --all-targets -- -D warnings
if errorlevel 1 goto :fail

echo.
echo [3/5] Tests (cargo test)
cargo test --locked
if errorlevel 1 goto :fail

echo.
echo [4/5] Dependencies (cargo deny: vulnerabilities, licenses, sources)
cargo deny --locked check
if errorlevel 1 goto :fail

echo.
echo [5/5] Release build (cargo build --release)
rem A running Horama built here locks its .exe and the build would fail with "Access denied".
powershell -NoProfile -Command "$t = Join-Path (Get-Location) 'target\*'; if (Get-Process horama -ErrorAction SilentlyContinue | Where-Object { $_.Path -like $t }) { exit 1 }"
if errorlevel 1 (
    echo Horama is running from target\: close it, then run the check again.
    goto :fail
)
cargo build --locked --release
if errorlevel 1 goto :fail

echo.
echo ALL CHECKS PASSED
exit /b 0

:fail
echo.
echo CHECK FAILED - see the output above
exit /b 1
