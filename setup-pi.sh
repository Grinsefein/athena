#!/bin/bash
# Athena Pi - Setup Script for Raspberry Pi
# Run this on your Pi to set up the downloader

set -e

echo "🍓 Athena Pi Setup for Raspberry Pi"
echo "===================================="

# Check if running on Pi
if [[ $(uname -m) != "arm"* && $(uname -m) != "aarch64" ]]; then
    echo "⚠️  Warning: This script is optimized for Raspberry Pi (ARM)"
    read -p "Continue anyway? (y/n) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

# Update system
echo "📦 Updating system packages..."
sudo apt update && sudo apt upgrade -y

# Install Python and dependencies
echo "🐍 Installing Python 3.12+ and dependencies..."
sudo apt install -y python3 python3-pip python3-venv ffmpeg

# Install Caddy (lightweight web server)
echo "🌐 Installing Caddy..."
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update
sudo apt install -y caddy

# Create download directory on USB (recommended)
echo "📁 Setting up download directory..."
DOWNLOAD_DIR="${DOWNLOAD_DIR:-$HOME/athena-downloads}"
mkdir -p "$DOWNLOAD_DIR"
echo "Downloads will go to: $DOWNLOAD_DIR"

# Create virtual environment
echo "🧪 Creating Python virtual environment..."
python3 -m venv .venv
source .venv/bin/activate

# Install Python packages
echo "⬇️  Installing Python dependencies..."
pip install -r requirements.txt

# Install yt-dlp with auto-update
echo "🎬 Installing yt-dlp..."
pip install -U yt-dlp

# Create systemd service for auto-start
echo "⚙️  Creating systemd service..."
sudo tee /etc/systemd/system/athena.service > /dev/null <<EOF
[Unit]
Description=Athena Pi YouTube Downloader
After=network.target

[Service]
Type=simple
User=$USER
WorkingDirectory=$(pwd)
Environment="DOWNLOAD_DIR=$DOWNLOAD_DIR"
ExecStart=$(pwd)/.venv/bin/uvicorn main:app --host 0.0.0.0 --port 8000 --workers 1
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF

# Copy Caddyfile
echo "🔧 Configuring Caddy..."
sudo cp Caddyfile /etc/caddy/Caddyfile
sudo systemctl reload caddy || sudo systemctl start caddy

# Enable services
echo "🚀 Enabling services..."
sudo systemctl daemon-reload
sudo systemctl enable athena
sudo systemctl enable caddy

echo ""
echo "✅ Setup complete!"
echo ""
echo "Usage:"
echo "  sudo systemctl start athena    # Start the downloader"
echo "  sudo systemctl stop athena     # Stop the downloader"
echo "  sudo systemctl status athena   # Check status"
echo ""
echo "Access the web interface at:"
echo "  http://$(hostname -I | awk '{print $1}'):8000"
echo "  or http://athena.local (if using Caddy + mDNS)"
echo ""
echo "📌 IMPORTANT: Set your download directory to a USB drive!"
echo "   Edit /etc/systemd/system/athena.service and change:"
echo "   Environment=\"DOWNLOAD_DIR=/path/to/usb/drive\""
echo ""
echo "🔒 For secure remote access, install Tailscale:"
echo "   curl -fsSL https://tailscale.com/install.sh | sh"
echo "   sudo tailscale up"
