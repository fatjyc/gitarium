RULESYNC ?= npx --yes rulesync@43.14.0

.PHONY: build test check fmt run rules

build:
	cargo build

test:
	cargo test

check:
	cargo check

fmt:
	cargo fmt

run:
	cargo run

rules:
	$(RULESYNC) generate
