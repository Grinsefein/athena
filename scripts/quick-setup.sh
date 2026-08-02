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

# Build project
echo "Building Athena Pi in debug mode..."
cargo build

echo "=== Quick Setup Completed Successfully! ==="
echo "You can now run 'make dev-server' or 'cargo run' to start the application."
