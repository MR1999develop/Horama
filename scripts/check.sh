#!/usr/bin/env bash
# Runs every check required before merging a branch into main (same steps as check.bat).
# Usage: scripts/check.sh   (from any folder)
# The whole output is also written to target/check.log.
# Exit code: 0 = all green, 1 = a check failed.
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p target

run_checks() {
    set -e
    trap 'echo; echo "CHECK FAILED - see the output above"; exit 1' ERR
    echo "Horama check - $(date '+%Y-%m-%d %H:%M:%S')"
    echo "Branch: $(git rev-parse --abbrev-ref HEAD 2>/dev/null)"
    echo "Commit: $(git rev-parse HEAD 2>/dev/null)"
    echo "Toolchain: $(rustc -V)"
    echo; echo "[1/4] Formatting (cargo fmt --check)"
    cargo fmt --all -- --check
    echo; echo "[2/4] Lints (cargo clippy, warnings are errors)"
    cargo clippy --all-targets -- -D warnings
    echo; echo "[3/4] Tests (cargo test)"
    cargo test
    echo; echo "[4/4] Release build (cargo build --release)"
    cargo build --release
    echo; echo "ALL CHECKS PASSED"
}

( run_checks ) 2>&1 | tee target/check.log
exit "${PIPESTATUS[0]}"
