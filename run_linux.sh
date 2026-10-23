#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if [ -d "$HOME/.cargo/bin" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: Rust (cargo) is not installed. Install it via https://rustup.rs"
    exit 1
fi

if [ -x "target/release/horama" ]; then
    exec "./target/release/horama" "$@"
else
    echo "Building and launching Horama (Rust + Slint) in Release mode..."
    cargo run --release -- "$@"
fi
