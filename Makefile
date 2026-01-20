.PHONY: check build release run test test-all clippy fmt logs

LOG_DIR = $(HOME)/.clemini/logs
LOG_FILE = $(LOG_DIR)/clemini.log.$(shell date +%Y-%m-%d)

check:
	cargo check

build:
	cargo build

release:
	cargo build --release

run:
	cargo run --

# Run unit tests only (fast, no API key required)
test:
	cargo test --lib
	cargo test --bin clemini
	cargo test --test event_ordering_tests

# Run all tests including integration tests (requires GEMINI_API_KEY)
test-all:
	cargo nextest run --run-ignored all

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
