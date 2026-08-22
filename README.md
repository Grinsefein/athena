# Athena Pi

A unified Rust-based video downloader with embedded Svelte frontend. Downloads are stored temporarily and auto-deleted.

## Features

- **Unified Stack**: Pure Rust backend + embedded HTML/JS frontend (no separate frontend server)
- **Temporary Storage**: Downloads saved to system temp directory, auto-deleted after serving
- YouTube video/audio downloading via yt-dlp
- Real-time progress tracking with Server-Sent Events (SSE)
- Concurrent download limiting with semaphore
- **Metadata Cache**: TTL-based in-memory cache for yt-dlp metadata, reused across analyze and download flows to reduce upstream requests
- Memory-efficient streaming downloads
- Dark mode UI

## Requirements

- Rust 1.70+
- Node.js 18+ and npm (for building the frontend)
- yt-dlp installed on system
- ffmpeg (for audio extraction)

## Installation & Setup

Athena Pi runs natively on your Linux host (such as a Raspberry Pi or any Linux server). Running natively on the host has several advantages: smaller resource footprint, faster startup times, and direct access to system storage and resources.

We have provided a `Makefile` and helper scripts to automate the entire installation and daemonization process.

### 1. Install System Dependencies
Athena Pi requires `ffmpeg` and `yt-dlp` to download and process videos. You can install all system dependencies automatically on Debian-based systems (like Raspberry Pi OS, Ubuntu, Debian):

```bash
sudo make install-deps
```
*Note: This script will install `ffmpeg` and the latest standalone `yt-dlp` binary (updatable via `yt-dlp -U`).*

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

#### Updating yt-dlp

The in-app update button (`/api/ytdlp-update`) runs `yt-dlp -U` inside the service
process. Since the service runs as the unprivileged `athena` user, it cannot replace
the root-owned binary in `/usr/local/bin`. The setup scripts therefore install a
second, athena-owned copy under `/var/lib/athena/bin/yt-dlp` and prepend it to the
service PATH (via a systemd drop-in). The web UI button updates that copy and works
without root.

For manual updates of the system-wide binary, run on the host:

```bash
sudo yt-dlp -U
```

### Updating the Service
After pulling new code, rebuild from source and reinstall the service (also updates yt-dlp). Run it as a normal (non-root) user — the script uses sudo only where needed:
```bash
make update-service
```

### Removing the Service
To fully uninstall Athena Pi (service, binary, config, and the `athena` user):
```bash
sudo make remove-service
```

---

## Configuration

Environment variables can be configured in `/etc/athena/athena.env` (for systemd service) or `.env` in the project root:
- `PORT` - Server port (default: 8000)
- `DOWNLOAD_DIR` - Override temp directory (default: `/tmp/athena-downloads`)
- `MAX_CONCURRENT_DOWNLOADS` - Max parallel downloads (default: 2)
- `MAX_CONCURRENT_ANALYZE` - Max parallel analyze calls, each spawns yt-dlp (default: 2)
- `META_CACHE_TTL_HOURS` - TTL for cached yt-dlp metadata in hours; 0 disables the cache (default: 6)
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
