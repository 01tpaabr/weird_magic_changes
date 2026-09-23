# Dev pipeline. Deliberately small: a game breaking is cheap, a slow loop is not.
#
#   make            -> build (dev profile: opt-level 1, Zig ReleaseSafe)
#   make run        -> build + run the app (ARGS="1000000 600")
#   make check      -> fmt-check + clippy + zig fmt-check   (fast, runs in pre-commit)
#   make test       -> Zig tests + Rust tests
#   make ci         -> check + test (what "green" means for this repo)
#   make bench      -> criterion benches
#   make release    -> optimized build (LTO, ReleaseFast)
#   make fmt        -> format everything in place
#   make setup      -> install toolchains + git hooks (idempotent)
#   make clean

SHELL := /bin/bash
.DEFAULT_GOAL := build
ZIG ?= zig
ARGS ?=

.PHONY: build run release check test ci bench fmt fmt-check lint zig-test zig-fmt-check setup hooks clean help

build:
	cargo build --workspace

run: build
	cargo run -p app --bin wmc -- $(ARGS)

release:
	cargo build --workspace --release

## quality gates -------------------------------------------------------------

check: fmt-check lint zig-fmt-check

fmt:
	cargo fmt --all
	cd zig && $(ZIG) fmt .

fmt-check:
	cargo fmt --all -- --check

zig-fmt-check:
	cd zig && $(ZIG) fmt --check .

lint:
	cargo clippy --workspace --all-targets -- -D warnings

## tests ----------------------------------------------------------------------

test: zig-test
	cargo test --workspace

zig-test:
	cd zig && $(ZIG) build test

ci: check test

bench:
	cargo bench -p sim-core

## environment ----------------------------------------------------------------

setup:
	./scripts/setup.sh

hooks:
	git config core.hooksPath .githooks
	chmod +x .githooks/*

clean:
	cargo clean
	rm -rf zig/zig-out zig/.zig-cache

help:
	@grep -E '^#   make' Makefile | sed 's/^#   //'
