@echo off
rem Runs every check required before merging a branch into main.
rem Usage: scripts\check.bat   (from any folder)
rem Exit code: 0 = all green, 1 = a check failed.
setlocal
cd /d "%~dp0.."

echo.
echo [1/4] Formatting (cargo fmt --check)
cargo fmt --all -- --check
if errorlevel 1 goto :fail

echo.
echo [2/4] Lints (cargo clippy, warnings are errors)
cargo clippy --all-targets -- -D warnings
if errorlevel 1 goto :fail

echo.
echo [3/4] Tests (cargo test)
cargo test
if errorlevel 1 goto :fail

echo.
echo [4/4] Release build (cargo build --release)
cargo build --release
if errorlevel 1 goto :fail

echo.
echo ALL CHECKS PASSED
exit /b 0

:fail
echo.
echo CHECK FAILED - see the output above
exit /b 1
