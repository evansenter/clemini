.PHONY: check build release test clippy fmt logs json-logs

LOG_DIR = $(HOME)/.clemini/logs
LOG_FILE = $(LOG_DIR)/clemini.log.$(shell date +%Y-%m-%d)
JSON_LOG_FILE = $(LOG_DIR)/clemini.json.$(shell date +%Y-%m-%d)

check:
	cargo check

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

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

json-logs:
	@if [ -f "$(JSON_LOG_FILE)" ]; then \
		tail -f "$(JSON_LOG_FILE)" | jq -r .; \
	else \
		echo "JSON log file not found: $(JSON_LOG_FILE)"; \
		exit 1; \
	fi
