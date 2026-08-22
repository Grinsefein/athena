#!/bin/bash
set -e

echo "=== Athena Pi Quick Setup ==="

# Check for .env file
if [ ! -f .env ]; then
    echo "Creating .env file from .env.example..."
    cp .env.example .env
else
    echo ".env file already exists."
fi

# Try to run install-linux.sh if user is root or has sudo
echo "Checking and installing dependencies..."
if [ -f scripts/install-linux.sh ]; then
    bash scripts/install-linux.sh
fi

# Build frontend
if ! command -v npm >/dev/null 2>&1; then
    echo "ERROR: npm not found. Node.js >= 18 and npm are required to build the frontend." >&2
    echo "       Install Node.js first (e.g. via nvm or NodeSource), then re-run this script." >&2
    exit 1
fi
echo "Building Svelte frontend..."
(cd frontend && npm install && npm run build)

# Build project
echo "Building Athena Pi in debug mode..."
cargo build

echo "=== Quick Setup Completed Successfully! ==="
echo "You can now run 'make dev-server' or 'cargo run' to start the application."
