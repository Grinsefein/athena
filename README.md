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

## Development

```bash
# Build
cargo build --release

# Run
cargo run
```

Server listens on `0.0.0.0:8000` by default.

## Configuration

Environment variables:
- `PORT` - Server port (default: 8000)
- `DOWNLOAD_DIR` - Override temp directory (default: system temp dir)
- `MAX_CONCURRENT_DOWNLOADS` - Max parallel downloads (default: 2)

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
