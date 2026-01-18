.PHONY: check build release test clippy fmt logs

LOG_DIR = $(HOME)/.clemini/logs
LOG_FILE = $(LOG_DIR)/clemini.log.$(shell date +%Y-%m-%d)

check:
	cargo check

build:
	cargo build

release:
	cargo build --release

# Run tests separately for clearer output when one type fails.
# Integration tests (confirmation_tests) are ignored by default; use --include-ignored for live API tests.
test:
	cargo test --lib
	cargo test --bin clemini
	cargo test --test confirmation_tests

clippy:
	cargo clippy -- -D warnings

fmt:
	cargo fmt

logs:
	@if [ -f "$(LOG_FILE)" ]; then \
		tail -f "$(LOG_FILE)"; \
	else \
		echo "Log file not found: $(LOG_FILE)"; \
		exit 1; \
	fi
