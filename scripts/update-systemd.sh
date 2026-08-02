#!/bin/bash
set -e

echo "=== Updating Athena Pi Service ==="

# 1. Refuse to build cargo targets as root (security/file-permission issues)
if [ "$(id -u)" = "0" ]; then
    echo "ERROR: Do not run this script as root." >&2
    echo "Please run it as a normal (non-root) user. Root access is requested with sudo when needed:" >&2
    echo "  make update-service" >&2
    exit 1
fi

# 2. Build the release binary
echo "Building optimized release binary..."
cargo build --release

# 3. Update yt-dlp to the latest version
echo "Updating yt-dlp..."
if command -v yt-dlp >/dev/null 2>&1; then
    sudo yt-dlp -U || echo "Warning: yt-dlp self-update failed (you can run 'sudo yt-dlp -U' manually)."
else
    echo "Warning: yt-dlp not found. Please run 'sudo make install-deps'."
fi

# 4. Stop the service before replacing the binary
echo "Stopping athena service..."
sudo systemctl stop athena || echo "Service not running, continuing..."

# 5. Install the new binary
echo "Installing new binary to /usr/local/bin..."
sudo cp target/release/athena /usr/local/bin/athena
sudo chmod +x /usr/local/bin/athena

# 6. Start the service
echo "Starting athena service..."
sudo systemctl start athena

echo "=== Athena Pi Service Updated Successfully! ==="
echo "Status of athena service:"
sudo systemctl status athena --no-pager | head -n 15
