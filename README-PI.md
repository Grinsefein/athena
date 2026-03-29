# Athena Pi 🍓

Lightweight YouTube video downloader optimized for **Raspberry Pi**. Uses **FastAPI** + **Alpine.js** for minimal memory footprint (~50MB RAM vs 300MB+ for Node.js/Next.js).

## Features

- ⚡ **Lightweight**: Python + Alpine.js (single HTML file)
- 📱 **Mobile-optimized**: Touch-friendly UI, works on all devices
- 🔄 **Real-time progress**: Server-Sent Events (SSE) instead of polling
- 💾 **Pi-friendly**: Configurable download directory (use USB, not SD card!)
- 🔒 **Secure**: Tailscale integration for private access
- 🌐 **HTTPS**: Automatic SSL with Caddy

## Quick Start

```bash
# 1. Clone and enter directory
cd athena

# 2. Run the Pi setup script
curl -fsSL https://raw.githubusercontent.com/Grinsefein/athena/main/setup-pi.sh | bash

# Or manually:
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
python main.py
```

## Pi-Specific Optimizations

| Aspect | Before (Next.js) | After (FastAPI) |
|--------|-----------------|----------------|
| **RAM Usage** | ~300-500MB | ~50-80MB |
| **CPU Usage** | High (build step) | Low (no build) |
| **Disk Writes** | Heavy (node_modules) | Minimal |
| **Startup Time** | Slow | Instant |
| **SD Card Wear** | High | Low |

## Directory Structure

```
athena/
├── main.py              # FastAPI backend + Alpine.js frontend (embedded)
├── requirements.txt     # Python dependencies
├── setup-pi.sh          # Automated Pi setup
├── Caddyfile           # Web server config
└── downloads/          # Download directory (set to USB!)
```

## Configuration

### Download Directory (IMPORTANT!)

**Never** download to the SD card - it will wear out quickly. Use a USB drive:

```bash
# Edit the service
sudo systemctl edit --full athena

# Change this line:
Environment="DOWNLOAD_DIR=/media/pi/USB_DRIVE/athena-downloads"

# Reload and restart
sudo systemctl daemon-reload
sudo systemctl restart athena
```

### Remote Access with Tailscale

```bash
# Install Tailscale
curl -fsSL https://tailscale.com/install.sh | sh

# Authenticate
sudo tailscale up

# Access from anywhere
# http://your-pi-name.your-account.ts.net
```

### Auto-Update yt-dlp

YouTube changes frequently. Add to crontab:

```bash
# Edit crontab
crontab -e

# Add line to update daily at 3am
0 3 * * * /home/pi/athena/.venv/bin/pip install -U yt-dlp
```

## Service Management

```bash
# Start/stop/restart
sudo systemctl start athena
sudo systemctl stop athena
sudo systemctl restart athena

# View logs
sudo journalctl -u athena -f

# Auto-start on boot
sudo systemctl enable athena
```

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Web UI (Alpine.js) |
| `/api/analyze` | POST | Extract video info |
| `/api/download` | POST | Start download |
| `/api/progress/{id}` | GET | SSE progress stream |
| `/api/file/{id}` | GET | Download completed file |

## Development

```bash
# Local development
source .venv/bin/activate
python main.py

# Access at http://localhost:8000
```

## Hardware Recommendations

- **Pi Model**: Pi 3B+ or newer (Zero 2 W works but slower)
- **RAM**: 1GB minimum, 2GB+ recommended
- **Storage**: USB 3.0 SSD or high-endurance microSD
- **Cooling**: Passive heatsink sufficient

## Troubleshooting

### yt-dlp outdated
```bash
source .venv/bin/activate
pip install -U yt-dlp
```

### Permission denied on downloads
```bash
# Fix permissions
sudo chown -R pi:pi /media/pi/USB_DRIVE/athena-downloads
```

### Port already in use
```bash
# Find and kill process
sudo lsof -i :8000
sudo kill <PID>
```

## License

MIT - Free for personal use. Respect copyright laws.
