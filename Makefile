# gitatlas-cli — multi-repo Git management CLI.
#
# `make check` is the pre-push gate and runs exactly what the CI `check` job runs
# (.github/workflows/ci.yml); `make hooks` wires it into git. Releases come from
# release.mk (release-kit): `make release` tags origin/main and pushes the tag.
.DEFAULT_GOAL := help

.PHONY: help check fmt fmt-check lint test build hooks clean

help: ## List targets
	@grep -hE '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{ printf "  %-16s %s\n", $$1, $$2 }'

check: fmt-check lint test ## The pre-push gate: the CI check job (fmt, clippy, test)
	@echo "All pre-commit checks passed."

fmt: ## Format the code
	cargo fmt --all

fmt-check: ## Fail if the code is not formatted
	cargo fmt --all --check

lint: ## Clippy with warnings denied, as CI runs it
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

test: ## Tests, as CI runs them
	cargo test --workspace --all-features --locked

build: ## Build the release binary
	cargo build --release

hooks: ## Install the prek git hooks (pre-commit and pre-push)
	prek install

clean: ## Remove build artifacts
	cargo clean

include release.mk
