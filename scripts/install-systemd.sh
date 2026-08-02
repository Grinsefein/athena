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

# 5. Create systemd service file
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

[Install]
WantedBy=multi-user.target
EOF

# 6. Enable and start the service
echo "Reloading systemd, enabling and starting athena service..."
sudo systemctl daemon-reload
sudo systemctl enable athena
sudo systemctl restart athena

echo "=== Athena Pi Service Successfully Installed and Started! ==="
echo "Status of athena service:"
sudo systemctl status athena --no-pager | head -n 15
