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
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d '"' -f2)
echo "Building frontend and optimized release binary (v${VERSION})..."
(cd frontend && npm install && npm run build)
cargo build --release

# 3. Update yt-dlp to the latest version
echo "Updating yt-dlp..."
if command -v yt-dlp >/dev/null 2>&1; then
    sudo yt-dlp -U || echo "Warning: yt-dlp self-update failed (you can run 'sudo yt-dlp -U' manually)."
else
    echo "Warning: yt-dlp not found. Please run 'sudo make install-deps'."
fi

# 3b. Ensure the service user has its own writable yt-dlp copy.
#     The web UI update button runs inside the athena service, which cannot
#     replace the root-owned binary in /usr/local/bin. This migration is
#     idempotent and safe to run on already-installed systems.
echo "Ensuring writable yt-dlp copy for the service user..."
sudo mkdir -p /var/lib/athena/bin
if [ ! -f /var/lib/athena/bin/yt-dlp ]; then
    if command -v yt-dlp >/dev/null 2>&1; then
        sudo cp "$(command -v yt-dlp)" /var/lib/athena/bin/yt-dlp
    else
        sudo curl -fL https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /var/lib/athena/bin/yt-dlp
    fi
fi
sudo curl -sfL https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /tmp/athena-ytdlp-new && \
    sudo mv /tmp/athena-ytdlp-new /var/lib/athena/bin/yt-dlp || \
    echo "Warning: could not download fresh yt-dlp, keeping existing copy."
sudo chown -R athena:athena /var/lib/athena 2>/dev/null || true
if [ -f /var/lib/athena/bin/yt-dlp ]; then
    sudo chmod 755 /var/lib/athena/bin/yt-dlp
else
    echo "Warning: writable yt-dlp copy for the service user is missing." >&2
fi

echo "Configuring service PATH override..."
sudo mkdir -p /etc/systemd/system/athena.service.d
sudo tee /etc/systemd/system/athena.service.d/override.conf > /dev/null <<EOF
[Service]
Environment=PATH=/var/lib/athena/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
EOF

# 4. Stop the service before replacing the binary
echo "Stopping athena service..."
sudo systemctl stop athena || echo "Service not running, continuing..."

# 5. Install the new binary
echo "Installing new binary to /usr/local/bin..."
sudo cp target/release/athena /usr/local/bin/athena
sudo chmod +x /usr/local/bin/athena

# 6. Start the service
echo "Starting athena service..."
sudo systemctl daemon-reload
sudo systemctl start athena

echo "=== Athena Pi Service Updated Successfully! ==="
echo "Status of athena service:"
sudo systemctl status athena --no-pager | head -n 15
