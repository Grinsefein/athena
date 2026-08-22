#!/bin/bash
set -e

echo "=== Installing Athena Pi Host Dependencies ==="

# Check if running on Debian/Ubuntu
if [ -f /etc/debian_version ] || [ -f /etc/lsb-release ]; then
    echo "Detected Debian/Ubuntu-based system. Installing packages via apt..."
    sudo apt-get update
    sudo apt-get install -y ffmpeg python3 python3-pip curl pkg-config libssl-dev build-essential nodejs npm
else
    echo "Non-Debian/Ubuntu system. Please ensure you have ffmpeg, python3, pip, pkg-config, nodejs and npm installed manually."
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

echo "Checking Node.js (>= 18 required for the frontend build)..."
if command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1; then
    NODE_MAJOR=$(node -p 'process.versions.node.split(".")[0]')
    if [ "$NODE_MAJOR" -lt 18 ]; then
        echo "WARNING: Node.js $(node --version) is too old for Vite (needs >= 18)."
        echo "         Install a newer version via NodeSource or nvm before running 'make build'."
    else
        node --version
        npm --version
    fi
else
    echo "WARNING: Node.js/npm not found. The frontend build ('make build'/'make release'/'make update-service') requires Node.js >= 18 + npm."
fi

echo "=== System Dependencies Installed Successfully ==="
