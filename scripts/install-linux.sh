#!/bin/bash
set -e

echo "=== Installing Athena Pi Host Dependencies ==="

# Check if running on Debian/Ubuntu
if [ -f /etc/debian_version ] || [ -f /etc/lsb-release ]; then
    echo "Detected Debian/Ubuntu-based system. Installing packages via apt..."
    sudo apt-get update
    sudo apt-get install -y ffmpeg python3 python3-pip python3-venv curl pkg-config libssl-dev build-essential
else
    echo "Non-Debian/Ubuntu system. Please ensure you have ffmpeg, python3, pip, and pkg-config installed manually."
fi

# Install/upgrade yt-dlp
echo "Installing/upgrading yt-dlp..."
# If pip3 is available, install yt-dlp
if command -v pip3 >/dev/null 2>&1; then
    # Try with --break-system-packages (newer python versions mandate this for global pip install)
    sudo pip3 install --upgrade --break-system-packages yt-dlp 2>/dev/null || sudo pip3 install --upgrade yt-dlp
else
    echo "pip3 not found! Downloading yt-dlp binary directly..."
    sudo curl -L https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /usr/local/bin/yt-dlp
    sudo chmod a+rx /usr/local/bin/yt-dlp
fi

echo "Verifying installations..."
ffmpeg -version | head -n 1
yt-dlp --version
cargo --version || echo "Rust is not installed. Please install Rust via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"

echo "=== System Dependencies Installed Successfully ==="
