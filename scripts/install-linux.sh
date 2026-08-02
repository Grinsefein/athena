#!/bin/bash
set -e

echo "=== Installing Athena Pi Host Dependencies ==="

# Check if running on Debian/Ubuntu
if [ -f /etc/debian_version ] || [ -f /etc/lsb-release ]; then
    echo "Detected Debian/Ubuntu-based system. Installing packages via apt..."
    sudo apt-get update
    sudo apt-get install -y ffmpeg python3 python3-pip curl pkg-config libssl-dev build-essential
else
    echo "Non-Debian/Ubuntu system. Please ensure you have ffmpeg, python3, pip, and pkg-config installed manually."
fi

# Install mutagen for yt-dlp metadata tagging
echo "Installing mutagen for metadata support..."
sudo pip3 install --break-system-packages mutagen || sudo pip install mutagen || pip install --user mutagen || true

# Install/upgrade yt-dlp (official standalone binary so self-update via `yt-dlp -U` works)
echo "Installing/upgrading yt-dlp..."
sudo curl -L https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /usr/local/bin/yt-dlp
sudo chmod a+rx /usr/local/bin/yt-dlp

echo "Verifying installations..."
ffmpeg -version | head -n 1
yt-dlp --version
cargo --version || echo "Rust is not installed. Please install Rust via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"

echo "=== System Dependencies Installed Successfully ==="
