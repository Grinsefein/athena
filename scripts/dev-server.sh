#!/bin/bash

echo "=== Starting Athena Pi Development Server ==="

# Load environment variables from .env if present
# (set -a auto-exports everything; sourcing handles quoted values with
# spaces/special chars, unlike the previous grep|xargs approach)
if [ -f .env ]; then
    echo "Loading environment variables from .env..."
    set -a
    . ./.env
    set +a
fi

# Set default env vars if not set
export PORT=${PORT:-8000}
export RUST_LOG=${RUST_LOG:-info}

echo "Server will start on http://localhost:$PORT"
echo "Frontend hot-reload (optional): cd frontend && npm run dev  (proxies /api to localhost:$PORT)"

# Check if --watch is passed
if [ "$1" == "--watch" ]; then
    if command -v cargo-watch >/dev/null 2>&1; then
        echo "Starting with cargo-watch..."
        cargo watch -x run
    else
        echo "cargo-watch is not installed. Installing cargo-watch..."
        cargo install cargo-watch
        echo "Starting with cargo-watch..."
        cargo watch -x run
    fi
else
    echo "Starting with cargo run..."
    cargo run
fi
