# Athena Pi

A unified Rust-based video downloader with embedded Alpine.js frontend. Downloads are stored temporarily and auto-deleted.

## Features

- **Unified Stack**: Pure Rust backend + embedded HTML/JS frontend (no separate frontend server)
- **Temporary Storage**: Downloads saved to system temp directory, auto-deleted after serving
- YouTube video/audio downloading via yt-dlp
- Real-time progress tracking with Server-Sent Events (SSE)
- Concurrent download limiting with semaphore
- Memory-efficient streaming downloads
- Dark mode UI

## Requirements

- Rust 1.70+
- yt-dlp installed on system
- ffmpeg (for audio extraction)

## Host-Only Installation & Setup (Non-Docker)

Athena Pi is fully optimized to run natively on your Linux host (such as a Raspberry Pi or any Linux server) instead of using Docker. Running natively on the host has several advantages: smaller resource footprint, faster startup times, and direct access to system storage and resources.

We have provided a `Makefile` and helper scripts to automate the entire installation and daemonization process.

### 1. Install System Dependencies
Athena Pi requires `ffmpeg` and `yt-dlp` to download and process videos. You can install all system dependencies automatically on Debian-based systems (like Raspberry Pi OS, Ubuntu, Debian):

```bash
sudo make install-deps
```
*Note: This script will install `ffmpeg`, `python3`, `pip`, and upgrade to the latest version of `yt-dlp`.*

### 2. Quick Setup
Run the quick setup command to copy the template configuration file (`.env.example` to `.env`) and build the application in debug mode:

```bash
make quick-setup
```

### 3. Development Server
To start the application for development with live-reloading (rebuilding automatically when files change), run:

```bash
make dev-server
```
Alternatively, to run without watch/live-reload:
```bash
make dev
```
The server listens on `http://localhost:8000` by default.

---

## Production Deployment with Systemd (Auto-Start on Reboot)

For a production environment or running Athena Pi continuously on your Raspberry Pi, you should run it as a systemd service. This ensures the application starts automatically when the system reboots and is restarted automatically if it crashes.

### Install as a Service
Run the following command to build the optimized release binary, create a dedicated secure system user `athena`, configure the environmental settings, and install/start the systemd service:

```bash
sudo make setup-service
```

### Managing the Service
Once installed, you can manage the Athena Pi service with standard `systemctl` commands:

- **Check status**: `sudo systemctl status athena`
- **Stop service**: `sudo systemctl stop athena`
- **Start service**: `sudo systemctl start athena`
- **Restart service**: `sudo systemctl restart athena`
- **View logs**: `journalctl -u athena -f`

---

## Alternative: Docker Deployment

If you still prefer to run Athena Pi inside a Docker container, you can use the provided Docker configurations.

### Docker Compose
To build and start the service with docker-compose:
```bash
docker-compose up -d --build
```

### Multi-Architecture Support (ARM64/Raspberry Pi) using Docker Buildx
```bash
docker buildx build --platform linux/amd64,linux/arm64 -t your-username/athena-pi:latest --push .
```

---

## Configuration

Environment variables can be configured in `/etc/athena/athena.env` (for systemd service) or `.env` in the project root:
- `PORT` - Server port (default: 8000)
- `DOWNLOAD_DIR` - Override temp directory (default: `/tmp/athena-downloads`)
- `MAX_CONCURRENT_DOWNLOADS` - Max parallel downloads (default: 2)
- `MAX_FILE_AGE_HOURS` - Max age of temp files in hours before deletion (default: 1.0)
- `CLEANUP_INTERVAL_SECONDS` - Cleanup check interval in seconds (default: 600)
- `RUST_LOG` - Logging level (default: info)
- `ATHENA_PASSWORD` - Password protection (optional)

## Storage Behavior

Files are stored temporarily:
- Default location: System temp directory (`/tmp/athena-downloads` on Linux)
- Files deleted automatically after download completes
- Cleanup runs every 10 minutes for orphaned files
- Max file age: 1 hour

## API Endpoints

- `POST /api/analyze` - Analyze video URL
- `POST /api/download` - Start download
- `GET /api/progress/:id` - SSE progress stream
- `GET /api/file/:id` - Download file (auto-deleted after)
