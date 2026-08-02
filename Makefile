.PHONY: help build release run dev test clean watch lint fmt install-deps setup-service cross-build-arm64 cross-build-armv7 quick-setup dev-server check

help:
	@echo ""
	@echo "Athena Pi - Development & Deployment Guide"
	@echo "==========================================="
	@echo ""
	@echo "$(GREEN)⚡ Quick Start:$(NC)"
	@echo "  make quick-setup      Complete development setup (dependencies + build)"
	@echo "  make dev              Build and run with dev options"
	@echo "  make dev-server       Start dev server with live reload (--watch)"
	@echo ""
	@echo "$(GREEN)🔨 Building:$(NC)"
	@echo "  make build            Build debug binary"
	@echo "  make release          Build optimized release binary"
	@echo "  make run              Run debug binary"
	@echo "  make watch            Watch and rebuild on changes"
	@echo ""
	@echo "$(GREEN)✓ Testing & Quality:$(NC)"
	@echo "  make test             Run test suite"
	@echo "  make lint             Run clippy linter"
	@echo "  make fmt              Format code with rustfmt"
	@echo "  make check            Check without building"
	@echo ""
	@echo "$(GREEN)🚀 Installation & Deployment:$(NC)"
	@echo "  make install-deps     Install system dependencies"
	@echo "  make setup-service    Install systemd service (auto-start on reboot)"
	@echo ""
	@echo "$(GREEN)🐹 Cross-Compilation:$(NC)"
	@echo "  make cross-build-arm64   Cross-compile for ARM64 (Raspberry Pi)"
	@echo "  make cross-build-armv7   Cross-compile for ARMv7 (32-bit Pi)"
	@echo ""
	@echo "$(GREEN)🧹 Maintenance:$(NC)"
	@echo "  make clean            Remove build artifacts"
	@echo "  make help             Show this message"
	@echo ""
	@echo "$(YELLOW)Examples:$(NC)"
	@echo "  # First-time setup for development"
	@echo "  make quick-setup"
	@echo "  make dev-server"
	@echo ""
	@echo "  # Production deployment on Linux"
	@echo "  make release"
	@echo "  sudo make setup-service"
	@echo ""
	@echo "  # Build for Raspberry Pi"
	@echo "  make cross-build-arm64"
	@echo ""

# Development targets
quick-setup:
	@chmod +x scripts/quick-setup.sh
	@./scripts/quick-setup.sh

dev-server:
	@chmod +x scripts/dev-server.sh
	@./scripts/dev-server.sh --watch

dev: quick-setup build run

build:
	@echo "Building debug binary..."
	@cargo build
	@echo "✓ Binary at: target/debug/athena"

release:
	@echo "Building optimized release binary..."
	@cargo build --release
	@echo "✓ Binary at: target/release/athena"

run: build
	@echo ""
	@cargo run

watch:
	@command -v cargo-watch >/dev/null 2>&1 || cargo install cargo-watch
	@echo "Watching for changes... (Press Ctrl+C to stop)"
	@cargo watch -x run

test:
	@echo "Running test suite..."
	@cargo test

# Code quality targets
fmt:
	@echo "Formatting code..."
	@cargo fmt
	@echo "✓ Done"

lint:
	@echo "Running clippy linter..."
	@cargo clippy -- -D warnings

check:
	@echo "Checking code (no build)..."
	@cargo check

# Setup and deployment targets
install-deps:
	@echo "Installing system dependencies..."
	@chmod +x scripts/install-linux.sh
	@./scripts/install-linux.sh

setup-service:
	@if [ "$$(id -u)" != "0" ]; then \
		echo "This target requires root. Run: sudo make setup-service"; \
		exit 1; \
	fi
	@echo "Installing systemd service with auto-start on reboot..."
	@chmod +x scripts/install-systemd.sh
	@./scripts/install-systemd.sh

# Cross-compilation targets
cross-build-arm64:
	@command -v cross >/dev/null 2>&1 || cargo install cross
	@echo "Building for ARM64 (Raspberry Pi 3/4/5)..."
	@cross build --release --target aarch64-unknown-linux-gnu
	@echo ""
	@echo "✓ Build complete!"
	@echo "  Binary: target/aarch64-unknown-linux-gnu/release/athena"
	@echo ""
	@echo "Next steps:"
	@echo "  1. Transfer binary to Raspberry Pi:"
	@echo "     scp target/aarch64-unknown-linux-gnu/release/athena pi@raspberrypi:/tmp/"
	@echo ""
	@echo "  2. SSH into Pi and install systemd service:"
	@echo "     ssh pi@raspberrypi 'cd athena && sudo make setup-service'"

cross-build-armv7:
	@command -v cross >/dev/null 2>&1 || cargo install cross
	@echo "Building for ARMv7 (32-bit Raspberry Pi)..."
	@cross build --release --target armv7-unknown-linux-gnueabihf
	@echo ""
	@echo "✓ Build complete!"
	@echo "  Binary: target/armv7-unknown-linux-gnueabihf/release/athena"
	@echo ""
	@echo "Next steps:"
	@echo "  1. Transfer binary to Raspberry Pi:"
	@echo "     scp target/armv7-unknown-linux-gnueabihf/release/athena pi@raspberrypi:/tmp/"
	@echo ""
	@echo "  2. SSH into Pi and install systemd service:"
	@echo "     ssh pi@raspberrypi 'cd athena && sudo make setup-service'"

# Maintenance targets
clean:
	@echo "Cleaning build artifacts..."
	@cargo clean
	@rm -rf .tmp/downloads 2>/dev/null || true
	@rm -rf /tmp/athena-downloads 2>/dev/null || true
	@echo "✓ Clean complete"

# Color codes for terminal output
GREEN := \033[0;32m
YELLOW := \033[1;33m
NC := \033[0m
