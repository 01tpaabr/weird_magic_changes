#!/usr/bin/env bash
# One-shot, idempotent environment setup for this repo (macOS + Homebrew assumed;
# adjust the zig line for Linux). Safe to re-run.
set -euo pipefail
cd "$(dirname "$0")/.."

need() { command -v "$1" >/dev/null 2>&1; }

echo "== rust"
if ! need rustup; then
  echo "rustup not found. Install from https://rustup.rs then re-run." >&2
  exit 1
fi
rustup component add rustfmt clippy >/dev/null
rustc --version && cargo --version

echo "== zig"
if ! need zig; then
  if need brew; then brew install zig; else
    echo "zig not found and no Homebrew. Install zig >= 0.16 from https://ziglang.org/download/" >&2
    exit 1
  fi
fi
zig version

echo "== git hooks"
git config core.hooksPath .githooks
chmod +x .githooks/*

echo "== smoke build"
make ci

echo
echo "ready. try: make run ARGS=\"100000 60\""
