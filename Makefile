.PHONY: help build release run test clean install-deps setup-service cross-build-arm64

help:
	@echo "Athena Pi - Available targets:"
	@echo ""
	@echo "  build              Build debug binary"
	@echo "  release            Build optimized release binary"
	@echo "  run                Run in development mode"
	@echo "  test               Run test suite"
	@echo "  watch              Watch and rebuild on file changes (requires cargo-watch)"
	@echo "  clean              Clean build artifacts"
	@echo "  install-deps       Install system dependencies (Linux)"
	@echo "  setup-service      Setup systemd service (requires sudo)"
	@echo "  cross-build-arm64  Cross-compile for ARM64/Raspberry Pi"
	@echo ""

build:
	cargo build

release:
	cargo build --release

run:
	cargo run

test:
	cargo test

watch:
	@command -v cargo-watch >/dev/null 2>&1 || cargo install cargo-watch
	cargo watch -x run

clean:
	cargo clean
	rm -rf /tmp/athena-downloads/*

fmt:
	cargo fmt

lint:
	cargo clippy -- -D warnings

install-deps:
	@chmod +x scripts/install-linux.sh
	@./scripts/install-linux.sh

setup-service:
	@if [ "$$EUID" -ne 0 ]; then \
		echo "This target requires root. Run: sudo make setup-service"; \
		exit 1; \
	fi
	@chmod +x scripts/setup-systemd.sh
	@scripts/setup-systemd.sh

cross-build-arm64:
	@command -v cross >/dev/null 2>&1 || cargo install cross
	cross build --release --target aarch64-unknown-linux-gnu
	@echo "Binary built for ARM64 at: target/aarch64-unknown-linux-gnu/release/athena"

cross-build-armv7:
	@command -v cross >/dev/null 2>&1 || cargo install cross
	cross build --release --target armv7-unknown-linux-gnueabihf
	@echo "Binary built for ARMv7 at: target/armv7-unknown-linux-gnueabihf/release/athena"
