#!/bin/bash
set -e

echo "=== Installing Athena Pi Systemd Service ==="

# 1. Verify release binary was built by non-root user
if [ ! -f target/release/athena ]; then
    echo "ERROR: Release binary 'target/release/athena' not found!" >&2
    echo "To avoid security risks and file permission issues, please do not build cargo targets as root." >&2
    echo "Please build the release binary as a normal (non-root) user first by running:" >&2
    echo "  make release" >&2
    echo "" >&2
    echo "Then install the systemd service by running:" >&2
    echo "  sudo make setup-service" >&2
    exit 1
fi

# 2. Copy binary to /usr/local/bin
echo "Installing binary to /usr/local/bin..."
sudo cp target/release/athena /usr/local/bin/athena
sudo chmod +x /usr/local/bin/athena

# 3. Create athena user and group if they don't exist
if ! getent group athena >/dev/null; then
    echo "Creating athena group..."
    sudo groupadd -r athena
fi

if ! getent passwd athena >/dev/null; then
    echo "Creating athena system user..."
    sudo useradd -r -g athena -d /var/empty -s /sbin/nologin -c "Athena Pi service user" athena
fi

# 4. Set up configuration file
echo "Setting up configuration in /etc/athena..."
sudo mkdir -p /etc/athena
if [ -f .env ]; then
    sudo cp .env /etc/athena/athena.env
elif [ -f .env.example ]; then
    sudo cp .env.example /etc/athena/athena.env
else
    sudo touch /etc/athena/athena.env
fi

# Ensure DOWNLOAD_DIR is configured and exists with correct permissions
sudo mkdir -p /tmp/athena-downloads
sudo chown -R athena:athena /tmp/athena-downloads

# Also chown the configuration directory
sudo chown -R athena:athena /etc/athena

# 5. Dedicated writable yt-dlp copy for the service user.
#    The web UI update button runs 'yt-dlp -U' inside the service process,
#    which must be able to REPLACE its own binary (file + directory).
#    A root-owned binary in /usr/local/bin cannot be replaced by the service
#    user, so we maintain an athena-owned copy under /var/lib/athena/bin.
echo "Installing writable yt-dlp copy for the service user..."
sudo mkdir -p /var/lib/athena/bin
if command -v yt-dlp >/dev/null 2>&1; then
    sudo cp "$(command -v yt-dlp)" /var/lib/athena/bin/yt-dlp
else
    sudo curl -fL https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /var/lib/athena/bin/yt-dlp
fi
sudo chown -R athena:athena /var/lib/athena
sudo chmod 755 /var/lib/athena/bin/yt-dlp

# 6. Make the service resolve the writable yt-dlp copy first (systemd drop-in)
echo "Configuring service PATH override..."
sudo mkdir -p /etc/systemd/system/athena.service.d
sudo tee /etc/systemd/system/athena.service.d/override.conf > /dev/null <<EOF
[Service]
Environment=PATH=/var/lib/athena/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
EOF

# 7. Create systemd service file
echo "Creating systemd service file..."
sudo tee /etc/systemd/system/athena.service > /dev/null <<EOF
[Unit]
Description=Athena Pi - Video Downloader
After=network.target

[Service]
Type=simple
User=athena
Group=athena
WorkingDirectory=/tmp
EnvironmentFile=-/etc/athena/athena.env
ExecStart=/usr/local/bin/athena
Restart=always
RestartSec=5
# Hardening: confine the server (and its yt-dlp/ffmpeg children) to exactly
# what it needs. DOWNLOAD_DIR defaults to /tmp/athena-downloads (recreated
# at startup, also works inside PrivateTmp); /var/lib/athena holds the
# writable yt-dlp copy for self-update ('yt-dlp -U').
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/athena /tmp/athena-downloads
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX

[Install]
WantedBy=multi-user.target
EOF

# 8. Enable and start the service
echo "Reloading systemd, enabling and starting athena service..."
sudo systemctl daemon-reload
sudo systemctl enable athena
sudo systemctl restart athena

echo "=== Athena Pi Service Successfully Installed and Started! ==="
echo "Status of athena service:"
sudo systemctl status athena --no-pager | head -n 15
