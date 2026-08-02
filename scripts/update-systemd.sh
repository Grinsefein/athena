#!/bin/bash
set -e

echo "=== Updating Athena Pi Service ==="

# 1. Verify release binary was built by non-root user
if [ ! -f target/release/athena ]; then
    echo "ERROR: Release binary 'target/release/athena' not found!" >&2
    echo "To avoid security risks and file permission issues, please do not build cargo targets as root." >&2
    echo "Please build the release binary as a normal (non-root) user first by running:" >&2
    echo "  make update-service" >&2
    exit 1
fi

# 2. Update yt-dlp to the latest version
echo "Updating yt-dlp..."
if command -v yt-dlp >/dev/null 2>&1; then
    sudo yt-dlp -U || echo "Warning: yt-dlp self-update failed (you can run 'sudo yt-dlp -U' manually)."
else
    echo "Warning: yt-dlp not found. Please run 'sudo make install-deps'."
fi

# 3. Stop the service before replacing the binary
echo "Stopping athena service..."
sudo systemctl stop athena || echo "Service not running, continuing..."

# 4. Install the new binary
echo "Installing new binary to /usr/local/bin..."
sudo cp target/release/athena /usr/local/bin/athena
sudo chmod +x /usr/local/bin/athena

# 5. Start the service
echo "Starting athena service..."
sudo systemctl start athena

echo "=== Athena Pi Service Updated Successfully! ==="
echo "Status of athena service:"
sudo systemctl status athena --no-pager | head -n 15
