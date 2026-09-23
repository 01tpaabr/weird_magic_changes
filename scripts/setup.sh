#!/usr/bin/env bash
# One-shot, idempotent environment setup for this repo. Safe to re-run.
set -euo pipefail
cd "$(dirname "$0")/.."

need() { command -v "$1" >/dev/null 2>&1; }

echo "== rust"
if ! need rustup; then
  echo "rustup not found. Install from https://rustup.rs then re-run." >&2
  exit 1
fi
rustup update stable >/dev/null
rustup component add rustfmt clippy >/dev/null
rustc --version && cargo --version

echo "== git hooks"
git config core.hooksPath .githooks
chmod +x .githooks/*

echo "== smoke build (first Bevy build takes a few minutes)"
make ci

echo
echo "ready. try: make run ARGS=\"show 80 24 42\""
