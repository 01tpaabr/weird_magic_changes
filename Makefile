# Dev pipeline. Deliberately small: a game breaking is cheap, a slow loop is not.
#
#   make            -> build (dev profile: opt-level 1, deps at opt-level 3, Bevy dynamically linked)
#   make run        -> build + run the app (ARGS="show 80 24 42" or ARGS="play saves/dev")
#   make check      -> fmt-check + clippy          (fast, runs in pre-commit)
#   make test       -> cargo test (includes the 1-thread vs N-thread determinism gate and every scenario)
#   make scenario-test -> run scenarios/ and scenarios/tests/ at 1 and 8 threads, with their reports
#   make ci         -> check + test (what "green" means for this repo)
#   make bench      -> criterion benches
#   make release    -> optimized build (LTO, static Bevy)
#   make fmt        -> format everything in place
#   make setup      -> install toolchains + git hooks (idempotent)
#   make clean
#
# Dev builds pass `--features dev`, which turns on Bevy's `dynamic_linking`: the
# engine becomes one shared library that is linked once, so an app-crate change
# relinks in a second or two instead of tens. Release and bench builds never use it.

SHELL := /bin/bash
.DEFAULT_GOAL := build
ARGS ?=
DEV := --features app/dev

.PHONY: build run release check test scenario-test ci bench fmt fmt-check lint setup hooks clean help

build:
	cargo build --workspace $(DEV)

run: build
	cargo run -p app --bin wmc $(DEV) -- $(ARGS)

release:
	cargo build --workspace --release

## quality gates -------------------------------------------------------------

check: fmt-check lint

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets $(DEV) -- -D warnings

## tests ----------------------------------------------------------------------

test:
	cargo test --workspace $(DEV)

scenario-test: build
	@for f in scenarios/*.scenario scenarios/tests/*.scenario; do \
		for t in 1 8; do \
			cargo run -q -p app --bin wmc $(DEV) -- scenario $$f --threads $$t || exit 1; \
		done; \
	done

ci: check test

bench:
	cargo bench --workspace

## environment ----------------------------------------------------------------

setup:
	./scripts/setup.sh

hooks:
	git config core.hooksPath .githooks
	chmod +x .githooks/*

clean:
	cargo clean

help:
	@grep -E '^#   make' Makefile | sed 's/^#   //'
