from fastapi import FastAPI, HTTPException, BackgroundTasks, Request
from fastapi.responses import StreamingResponse, FileResponse, HTMLResponse, JSONResponse
from pydantic import BaseModel, HttpUrl
import yt_dlp
import asyncio
import json
import os
import uuid
import time
from pathlib import Path
from typing import Optional
import logging
from logging.handlers import RotatingFileHandler

# Set up Production Logging
log_formatter = logging.Formatter('%(asctime)s - %(name)s - %(levelname)s - %(message)s')
log_handler = RotatingFileHandler('athena.log', maxBytes=5*1024*1024, backupCount=2)
log_handler.setFormatter(log_formatter)

logger = logging.getLogger("athena")
logger.setLevel(logging.INFO)
logger.addHandler(log_handler)

console_handler = logging.StreamHandler()
console_handler.setFormatter(log_formatter)
logger.addHandler(console_handler)

app = FastAPI(title="Athena Pi", version="1.0.0", docs_url=None, redoc_url=None) # Hide docs in prod

@app.exception_handler(Exception)
async def global_exception_handler(request: Request, exc: Exception):
    logger.error(f"Global exception: {exc}", exc_info=True)
    return JSONResponse(
        status_code=500,
        content={"success": False, "error": "Internal server error occurred."}
    )

# Configuration
DOWNLOAD_DIR = Path(os.getenv("DOWNLOAD_DIR", "./downloads"))
DOWNLOAD_DIR.mkdir(exist_ok=True)
MAX_FILE_AGE_HOURS = 24
MAX_CONCURRENT_DOWNLOADS = int(os.getenv("MAX_CONCURRENT_DOWNLOADS", "2"))

# Active downloads storage & Semaphore
active_downloads = {}
download_semaphore = None


class DownloadRequest(BaseModel):
    url: HttpUrl
    format: str = "mp4"
    quality: str = "best"


class VideoInfo(BaseModel):
    id: str
    title: str
    description: str
    thumbnail: str
    duration: str
    author: str
    formats: list


@app.get("/")
async def root():
    """Serve the Alpine.js frontend"""
    return HTMLResponse(content=INDEX_HTML)


@app.post("/api/analyze")
async def analyze_video(request: DownloadRequest):
    """Extract video info without downloading"""
    try:
        ydl_opts = {
            "quiet": True,
            "no_warnings": True,
            "extract_flat": False,
        }
        
        with yt_dlp.YoutubeDL(ydl_opts) as ydl:
            info = ydl.extract_info(str(request.url), download=False)
            
        # Format duration
        duration_mins = info.get("duration", 0) // 60
        duration_secs = info.get("duration", 0) % 60
        duration = f"{duration_mins}:{duration_secs:02d}"
        
        # Extract formats
        video_formats = []
        audio_formats = []
        seen_qualities = set()
        
        for fmt in info.get("formats", []):
            if fmt.get("vcodec") != "none" and fmt.get("resolution"):
                quality_key = f"{fmt.get('resolution', 'unknown')}-{fmt.get('ext', 'unknown')}"
                if quality_key not in seen_qualities:
                    seen_qualities.add(quality_key)
                    video_formats.append({
                        "format": fmt.get("ext"),
                        "quality": fmt.get("format_note", fmt.get("resolution")),
                        "label": f"{fmt.get('resolution', 'unknown')} ({fmt.get('ext')})"
                    })
            
            if fmt.get("acodec") != "none" and fmt.get("vcodec") == "none":
                if "mp3" not in [a["format"] for a in audio_formats]:
                    audio_formats.append({
                        "format": "mp3",
                        "quality": "best",
                        "label": "MP3 Audio (Best)"
                    })
        
        # Deduplicate and limit
        unique_videos = video_formats[:4]
        formats = unique_videos + audio_formats[:1]
        
        return {
            "success": True,
            "data": {
                "id": info.get("id"),
                "title": info.get("title", "Unknown"),
                "description": info.get("description", "")[:200],
                "thumbnail": info.get("thumbnail", ""),
                "duration": duration,
                "author": info.get("uploader", "Unknown"),
                "formats": formats
            }
        }
        
    except Exception as e:
        logger.error(f"Analyze error: {e}")
        raise HTTPException(status_code=400, detail=str(e))


@app.post("/api/download")
async def start_download(request: DownloadRequest, background_tasks: BackgroundTasks):
    """Start a download and return ID for SSE tracking"""
    download_id = str(uuid.uuid4())[:8]
    
    active_downloads[download_id] = {
        "status": "queued",
        "progress": 0,
        "file_path": None,
        "file_name": None,
        "error": None,
        "timestamp": time.time()
    }
    
    # Start download in background via queue
    background_tasks.add_task(
        queued_download_task,
        download_id,
        str(request.url),
        request.format,
        request.quality
    )
    
    return {
        "success": True,
        "data": {
            "downloadId": download_id,
            "status": "queued"
        }
    }


@app.get("/api/progress/{download_id}")
async def progress_stream(download_id: str):
    """SSE endpoint for download progress"""
    async def event_generator():
        while True:
            if download_id not in active_downloads:
                yield f"data: {json.dumps({'error': 'Download not found'})}\n\n"
                break
            
            data = active_downloads[download_id].copy()
            
            # Add download URL if completed
            if data["status"] == "completed" and data["file_path"]:
                data["downloadUrl"] = f"/api/file/{download_id}"
            
            yield f"data: {json.dumps(data)}\n\n"
            
            if data["status"] in ["completed", "error"]:
                break
            
            await asyncio.sleep(0.5)
    
    return StreamingResponse(
        event_generator(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
        }
    )


@app.get("/api/file/{download_id}")
async def download_file(download_id: str):
    """Download the completed file"""
    if download_id not in active_downloads:
        raise HTTPException(status_code=404, detail="Download not found")
    
    download = active_downloads[download_id]
    if download["status"] != "completed" or not download["file_path"]:
        raise HTTPException(status_code=400, detail="Download not ready")
    
    file_path = Path(download["file_path"])
    if not file_path.exists():
        raise HTTPException(status_code=404, detail="File not found")
    
    return FileResponse(
        path=file_path,
        filename=download["file_name"],
        media_type="application/octet-stream"
    )


async def queued_download_task(download_id: str, url: str, format_type: str, quality: str):
    """Enforce concurrency control before blocking yt-dlp"""
    try:
        logger.info(f"Download {download_id} waiting in queue...")
        async with download_semaphore:
            if download_id in active_downloads:
                active_downloads[download_id]["status"] = "processing"
            logger.info(f"Download {download_id} starting processing...")
            await asyncio.to_thread(download_video_task, download_id, url, format_type, quality)
    except Exception as e:
        logger.error(f"Task error for {download_id}: {e}", exc_info=True)
        if download_id in active_downloads:
            active_downloads[download_id]["status"] = "error"
            active_downloads[download_id]["error"] = str(e)


def download_video_task(download_id: str, url: str, format_type: str, quality: str):
    """Background task to download video with progress updates"""
    try:
        output_template = str(DOWNLOAD_DIR / "%(title)s.%(ext)s")
        
        def progress_hook(d):
            if d["status"] == "downloading":
                total = d.get("total_bytes") or d.get("total_bytes_estimate", 0)
                downloaded = d.get("downloaded_bytes", 0)
                if total > 0:
                    percent = (downloaded / total) * 100
                    active_downloads[download_id]["progress"] = round(percent, 1)
                    
            elif d["status"] == "finished":
                active_downloads[download_id]["progress"] = 100
        
        # Build format string
        if format_type == "mp3":
            format_arg = "bestaudio/best"
            postprocessors = [{
                "key": "FFmpegExtractAudio",
                "preferredcodec": "mp3",
                "preferredquality": "192",
            }]
        elif quality == "best":
            format_arg = f"best[ext={format_type}]/best"
            postprocessors = []
        else:
            height = "".join(filter(str.isdigit, quality))
            format_arg = f"best[height<={height}][ext={format_type}]/best[height<={height}]/best"
            postprocessors = []
        
        ydl_opts = {
            "format": format_arg,
            "outtmpl": output_template,
            "progress_hooks": [progress_hook],
            "quiet": True,
            "no_warnings": True,
            "postprocessors": postprocessors,
        }
        
        with yt_dlp.YoutubeDL(ydl_opts) as ydl:
            info = ydl.extract_info(url, download=True)
            
            # Get the actual filename
            if format_type == "mp3":
                ext = "mp3"
            else:
                ext = info.get("ext", format_type)
            
            file_name = f"{info.get('title', 'download')}.{ext}"
            # Sanitize filename
            file_name = "".join(c for c in file_name if c.isalnum() or c in " ._-").rstrip()
            
            # Find the actual downloaded file
            base_path = DOWNLOAD_DIR / f"{info.get('title', 'download')}"
            possible_files = list(DOWNLOAD_DIR.glob(f"{info.get('title', 'download')}*"))
            
            if possible_files:
                actual_file = possible_files[0]
                active_downloads[download_id]["file_path"] = str(actual_file)
                active_downloads[download_id]["file_name"] = actual_file.name
            
        active_downloads[download_id]["status"] = "completed"
        logger.info(f"Download {download_id} completed")
        
    except Exception as e:
        logger.error(f"Download {download_id} failed: {e}")
        active_downloads[download_id]["status"] = "error"
        active_downloads[download_id]["error"] = str(e)


# Cleanup old files and memory periodically
@app.on_event("startup")
async def startup_event():
    """Start periodic cleanup task"""
    global download_semaphore
    download_semaphore = asyncio.Semaphore(MAX_CONCURRENT_DOWNLOADS)
    asyncio.create_task(periodic_cleanup())


async def periodic_cleanup():
    while True:
        cleanup_old_data()
        await asyncio.sleep(3600)  # Run every hour


def cleanup_old_data():
    """Remove files and dict entries older than MAX_FILE_AGE_HOURS"""
    try:
        current_time = time.time()
        
        # 1. Cleanup old files
        for file_path in DOWNLOAD_DIR.iterdir():
            if file_path.is_file():
                file_age_hours = (current_time - file_path.stat().st_mtime) / 3600
                if file_age_hours > MAX_FILE_AGE_HOURS:
                    file_path.unlink()
                    logger.info(f"Cleaned up old file: {file_path}")
                    
        # 2. Cleanup old memory records -> prevents RAM leak over months of Pi uptime
        stale_ids = []
        for did, dinfo in active_downloads.items():
            age_hours = (current_time - dinfo.get("timestamp", current_time)) / 3600
            if age_hours > MAX_FILE_AGE_HOURS:
                stale_ids.append(did)
                
        for did in stale_ids:
            del active_downloads[did]
            logger.info(f"Cleaned up stale memory entry: {did}")
            
    except Exception as e:
        logger.error(f"Cleanup error: {e}")


# Alpine.js Frontend (embedded for single-file deployment)
INDEX_HTML = """
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no">
    <title>Athena Pi</title>
    <script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js"></script>
    <link rel="icon" type="image/svg+xml" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'%3E%3Crect width='100' height='100' rx='20' fill='%236366f1'/%3E%3Cpath d='M35 30 L35 70 L75 50 Z' fill='white'/%3E%3C/svg%3E">
    <style>
        *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
        html, body { height: 100%; }
        body { 
            font-family: system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: #f9fafb;
        }
        body.dark { background: #111827; }
        [x-cloak] { display: none !important; }
        
        .container {
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            padding: 16px;
        }
        
        .card {
            width: 100%;
            max-width: 400px;
            background: white;
            border-radius: 16px;
            box-shadow: 0 10px 25px -5px rgba(0,0,0,0.1), 0 8px 10px -6px rgba(0,0,0,0.1);
            border: 1px solid #f3f4f6;
            padding: 24px;
        }
        .dark .card { background: #1f2937; border-color: #374151; }
        
        .header {
            text-align: center;
            margin-bottom: 24px;
        }
        
        .logo {
            display: inline-flex;
            align-items: center;
            justify-content: center;
            width: 48px;
            height: 48px;
            border-radius: 12px;
            background: #4f46e5;
            color: white;
            margin-bottom: 12px;
        }
        
        h1 {
            font-size: 24px;
            font-weight: 700;
            color: #111827;
            margin-bottom: 4px;
        }
        .dark h1 { color: white; }
        
        .subtitle {
            font-size: 14px;
            color: #6b7280;
        }
        .dark .subtitle { color: #9ca3af; }
        
        .input-wrap {
            position: relative;
            margin-bottom: 16px;
        }
        
        input[type="url"] {
            width: 100%;
            padding: 12px 44px 12px 16px;
            border-radius: 12px;
            border: 1px solid #e5e7eb;
            background: #f9fafb;
            font-size: 15px;
            outline: none;
            color: #1f2937;
        }
        input[type="url"]:focus {
            border-color: #4f46e5;
            box-shadow: 0 0 0 3px rgba(79, 70, 229, 0.1);
        }
        .dark input[type="url"] {
            background: #111827;
            border-color: #4b5563;
            color: #f3f4f6;
        }
        input[type="url"]:disabled { opacity: 0.6; cursor: not-allowed; }
        
        .input-btn {
            position: absolute;
            right: 6px;
            top: 6px;
            padding: 6px;
            border-radius: 8px;
            background: #4f46e5;
            border: none;
            color: white;
            cursor: pointer;
        }
        .input-btn:hover { background: #4338ca; }
        .input-btn:disabled { opacity: 0.5; cursor: not-allowed; }
        
        .error-box {
            text-align: center;
            font-size: 14px;
            color: #dc2626;
            background: #fef2f2;
            padding: 8px 12px;
            border-radius: 8px;
            margin-bottom: 12px;
        }
        .dark .error-box {
            color: #f87171;
            background: rgba(220, 38, 38, 0.1);
        }
        
        .video-info {
            display: flex;
            align-items: center;
            gap: 12px;
            padding: 8px;
            border-radius: 12px;
            background: #f9fafb;
            margin-bottom: 12px;
        }
        .dark .video-info { background: #111827; }
        
        .thumb {
            width: 64px;
            height: 64px;
            border-radius: 8px;
            overflow: hidden;
            background: #e5e7eb;
            flex-shrink: 0;
        }
        .dark .thumb { background: #374151; }
        .thumb img { width: 100%; height: 100%; object-fit: cover; }
        
        .video-meta { min-width: 0; }
        .video-title {
            font-weight: 600;
            font-size: 14px;
            color: #1f2937;
            white-space: nowrap;
            overflow: hidden;
            text-overflow: ellipsis;
        }
        .dark .video-title { color: #f3f4f6; }
        .video-author {
            font-size: 12px;
            color: #6b7280;
            white-space: nowrap;
            overflow: hidden;
            text-overflow: ellipsis;
        }
        .dark .video-author { color: #9ca3af; }
        
        .select-row {
            display: grid;
            grid-template-columns: 1fr 1fr;
            gap: 8px;
            margin-bottom: 12px;
        }
        
        .dropdown {
            position: relative;
        }
        .dropdown-trigger {
            width: 100%;
            display: flex;
            align-items: center;
            justify-content: space-between;
            padding: 10px 12px;
            border-radius: 10px;
            border: 1px solid #e5e7eb;
            background: white;
            font-size: 14px;
            color: #374151;
            cursor: pointer;
            transition: all 0.15s ease;
        }
        .dropdown-trigger:hover {
            border-color: #d1d5db;
            background: #f9fafb;
        }
        .dropdown-trigger:focus {
            outline: none;
            border-color: #4f46e5;
            box-shadow: 0 0 0 3px rgba(79, 70, 229, 0.1);
        }
        .dark .dropdown-trigger {
            background: #1f2937;
            border-color: #4b5563;
            color: #e5e7eb;
        }
        .dark .dropdown-trigger:hover {
            background: #374151;
            border-color: #6b7280;
        }
        .dropdown-trigger svg {
            width: 16px;
            height: 16px;
            color: #9ca3af;
            transition: transform 0.15s ease;
        }
        .dropdown-trigger.active svg {
            transform: rotate(180deg);
        }
        
        .dropdown-menu {
            position: absolute;
            top: calc(100% + 4px);
            left: 0;
            right: 0;
            background: white;
            border: 1px solid #e5e7eb;
            border-radius: 10px;
            box-shadow: 0 10px 25px -5px rgba(0,0,0,0.15), 0 8px 10px -6px rgba(0,0,0,0.1);
            z-index: 100;
            max-height: 200px;
            overflow-y: auto;
        }
        .dark .dropdown-menu {
            background: #1f2937;
            border-color: #4b5563;
        }
        
        .dropdown-item {
            padding: 10px 12px;
            font-size: 14px;
            color: #374151;
            cursor: pointer;
            transition: background 0.15s ease;
        }
        .dropdown-item:hover {
            background: #f3f4f6;
        }
        .dropdown-item.selected {
            background: #eef2ff;
            color: #4f46e5;
            font-weight: 500;
        }
        .dark .dropdown-item {
            color: #e5e7eb;
        }
        .dark .dropdown-item:hover {
            background: #374151;
        }
        .dark .dropdown-item.selected {
            background: rgba(79, 70, 229, 0.2);
            color: #818cf8;
        }
        
        .btn {
            position: relative;
            width: 100%;
            padding: 12px;
            border-radius: 12px;
            border: none;
            font-weight: 600;
            font-size: 14px;
            color: white;
            cursor: pointer;
            display: flex;
            align-items: center;
            justify-content: center;
            gap: 8px;
            overflow: hidden;
        }
        .btn:disabled { cursor: not-allowed; }
        .btn-primary { background: #4f46e5; }
        .btn-primary:hover:not(:disabled) { background: #4338ca; }
        .btn-primary:disabled { background: #818cf8; }
        
        .progress-fill {
            position: absolute;
            left: 0;
            top: 0;
            bottom: 0;
            background: #3730a3;
            transition: width 0.3s ease;
        }
        .btn-content { position: relative; display: flex; align-items: center; gap: 8px; }
        
        .btn-row {
            display: flex;
            gap: 8px;
            margin-top: 12px;
        }
        .btn-success {
            flex: 1;
            padding: 10px;
            border-radius: 10px;
            background: #059669;
            color: white;
            text-decoration: none;
            text-align: center;
            font-weight: 600;
            font-size: 14px;
        }
        .btn-success:hover { background: #047857; }
        .btn-secondary {
            padding: 10px 16px;
            border-radius: 10px;
            border: none;
            background: #f3f4f6;
            color: #374151;
            font-weight: 600;
            font-size: 14px;
            cursor: pointer;
        }
        .btn-secondary:hover { background: #e5e7eb; }
        .dark .btn-secondary { background: #374151; color: #e5e7eb; }
        .dark .btn-secondary:hover { background: #4b5563; }
        
        .theme-btn {
            position: fixed;
            top: 16px;
            right: 16px;
            padding: 8px;
            border-radius: 8px;
            background: white;
            border: 1px solid #e5e7eb;
            box-shadow: 0 1px 3px rgba(0,0,0,0.1);
            cursor: pointer;
            z-index: 50;
        }
        .theme-btn:hover { transform: scale(1.05); }
        .dark .theme-btn { background: #1f2937; border-color: #374151; }
        
        @keyframes spin {
            from { transform: rotate(0deg); }
            to { transform: rotate(360deg); }
        }
        .animate-spin { animation: spin 1s linear infinite; }
        
        @keyframes pulse {
            0%, 100% { opacity: 1; }
            50% { opacity: 0.5; }
        }
        .animate-pulse { animation: pulse 2s cubic-bezier(0.4, 0, 0.6, 1) infinite; }
    </style>
</head>
<body x-data="athenaApp()" x-init="init()">
    
    <button @click="toggleTheme()" class="theme-btn">
        <svg x-show="!darkMode" width="20" height="20" fill="none" stroke="#4f46e5" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M20.354 15.354A9 9 0 018.646 3.646 9.003 9.003 0 0012 21a9.003 9.003 0 008.354-5.646z"/></svg>
        <svg x-show="darkMode" width="20" height="20" fill="none" stroke="#eab308" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M12 3v1m0 16v1m9-9h-1M4 12H3m15.364 6.364l-.707-.707M6.343 6.343l-.707-.707m12.728 0l-.707.707M6.343 17.657l-.707.707M16 12a4 4 0 11-8 0 4 4 0 018 0z"/></svg>
    </button>

    <div class="container">
        <div class="card">
            <div class="header">
                <div class="logo">
                    <svg width="24" height="24" viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>
                </div>
                <h1>Athena</h1>
                <p class="subtitle">Pi Downloader</p>
            </div>

            <div class="input-wrap">
                <input 
                    type="url" 
                    x-model="url"
                    @keydown.enter="analyze()"
                    placeholder="Paste YouTube link..."
                    :disabled="loading"
                >
                <button 
                    @click="analyze()"
                    :disabled="!url || loading"
                    class="input-btn"
                >
                    <svg x-show="loading" class="animate-spin" width="20" height="20" viewBox="0 0 24 24" fill="none">
                        <circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" opacity="0.25"></circle>
                        <path fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" opacity="0.75"></path>
                    </svg>
                    <svg x-show="!loading" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M14 5l7 7m0 0l-7 7m7-7H3"/></svg>
                </button>
            </div>

            <div x-show="error" x-transition class="error-box" x-text="error"></div>

            <div x-show="videoInfo" x-transition style="margin-top: 16px;" x-cloak>
                <div class="video-info">
                    <div class="thumb">
                        <img :src="videoInfo?.thumbnail" alt="">
                    </div>
                    <div class="video-meta">
                        <div class="video-title" x-text="videoInfo?.title"></div>
                        <div class="video-author" x-text="videoInfo?.author"></div>
                    </div>
                </div>

                <div class="select-row">
                    <div class="dropdown" x-data="{ open: false }" @click.away="open = false">
                        <input type="hidden" x-model="format">
                        <button class="dropdown-trigger" :class="{ active: open }" @click="open = !open">
                            <span x-text="format === 'mp4' ? 'Video (MP4)' : format === 'mp3' ? 'Audio (MP3)' : 'Video (WebM)'"></span>
                            <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"/></svg>
                        </button>
                        <div x-show="open" x-transition class="dropdown-menu">
                            <div class="dropdown-item" :class="{ selected: format === 'mp4' }" @click="format = 'mp4'; open = false">Video (MP4)</div>
                            <div class="dropdown-item" :class="{ selected: format === 'mp3' }" @click="format = 'mp3'; open = false">Audio (MP3)</div>
                            <div class="dropdown-item" :class="{ selected: format === 'webm' }" @click="format = 'webm'; open = false">Video (WebM)</div>
                        </div>
                    </div>
                    <div class="dropdown" x-data="{ open: false }" @click.away="open = false">
                        <input type="hidden" x-model="quality">
                        <button class="dropdown-trigger" :class="{ active: open }" @click="open = !open">
                            <span x-text="quality === 'best' ? 'Highest' : quality"></span>
                            <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"/></svg>
                        </button>
                        <div x-show="open" x-transition class="dropdown-menu">
                            <div class="dropdown-item" :class="{ selected: quality === 'best' }" @click="quality = 'best'; open = false">Highest</div>
                            <template x-for="fmt in videoInfo?.formats || []" :key="fmt.quality">
                                <div class="dropdown-item" :class="{ selected: quality === fmt.quality }" @click="quality = fmt.quality; open = false" x-text="fmt.label"></div>
                            </template>
                        </div>
                    </div>
                </div>

                <button 
                    @click="download()"
                    :disabled="downloading || completed"
                    class="btn btn-primary"
                >
                    <div x-show="downloading && !queued" class="progress-fill" :style="`width: ${progress}%`"></div>
                    <div x-show="queued" class="progress-fill animate-pulse" style="width: 100%; background: #eab308;"></div>
                    <span class="btn-content">
                        <svg x-show="downloading" class="animate-spin" width="16" height="16" viewBox="0 0 24 24" fill="none">
                            <circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" opacity="0.25"></circle>
                            <path fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" opacity="0.75"></path>
                        </svg>
                        <svg x-show="!downloading && !completed" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4"/></svg>
                        <svg x-show="completed" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7"/></svg>
                        <span x-text="downloading ? (queued ? 'Queued...' : `${progress}%`) : (completed ? 'Done!' : 'Download')"></span>
                    </span>
                </button>

                <div x-show="completed" x-transition class="btn-row">
                    <a :href="downloadUrl" download class="btn-success">Save File</a>
                    <button @click="reset()" class="btn-secondary">New</button>
                </div>
            </div>
        </div>
    </div>

    <script>
        function athenaApp() {
            return {
                url: '',
                format: 'mp4',
                quality: 'best',
                loading: false,
                downloading: false,
                completed: false,
                queued: false,
                progress: 0,
                error: null,
                videoInfo: null,
                downloadId: null,
                downloadUrl: null,
                darkMode: false,
                eventSource: null,

                init() {
                    this.darkMode = localStorage.getItem('darkMode') === 'true' || (!('darkMode' in localStorage) && window.matchMedia('(prefers-color-scheme: dark)').matches);
                    if (this.darkMode) document.body.classList.add('dark');
                },

                toggleTheme() {
                    this.darkMode = !this.darkMode;
                    localStorage.setItem('darkMode', this.darkMode);
                    document.body.classList.toggle('dark', this.darkMode);
                },

                async analyze() {
                    if (!this.url) return;
                    this.loading = true;
                    this.error = null;

                    try {
                        const response = await fetch('/api/analyze', {
                            method: 'POST',
                            headers: { 'Content-Type': 'application/json' },
                            body: JSON.stringify({ url: this.url, format: this.format })
                        });
                        const data = await response.json();
                        if (!data.success) throw new Error('Failed to analyze');
                        this.videoInfo = data.data;
                    } catch (e) {
                        this.error = 'Invalid link. Check URL.';
                    } finally {
                        this.loading = false;
                    }
                },

                async download() {
                    if (!this.videoInfo) return;
                    this.downloading = true;
                    this.error = null;

                    try {
                        const response = await fetch('/api/download', {
                            method: 'POST',
                            headers: { 'Content-Type': 'application/json' },
                            body: JSON.stringify({ url: this.url, format: this.format, quality: this.quality })
                        });
                        const data = await response.json();
                        if (!data.success) throw new Error('Failed to start');
                        this.downloadId = data.data.downloadId;
                        this.queued = data.data.status === 'queued';
                        this.connectSSE();
                    } catch (e) {
                        this.error = 'Failed to start download.';
                        this.downloading = false;
                    }
                },

                connectSSE() {
                    if (this.eventSource) this.eventSource.close();
                    this.eventSource = new EventSource(`/api/progress/${this.downloadId}`);
                    
                    this.eventSource.onmessage = (event) => {
                        const data = JSON.parse(event.data);
                        if (data.error) {
                            this.error = data.error;
                            this.downloading = false;
                            this.eventSource.close();
                            return;
                        }
                        if (data.status === 'queued') this.queued = true;
                        else if (data.status === 'processing') this.queued = false;
                        this.progress = data.progress || 0;

                        if (data.status === 'completed') {
                            this.downloading = false;
                            this.queued = false;
                            this.completed = true;
                            this.downloadUrl = data.downloadUrl;
                            this.eventSource.close();
                        } else if (data.status === 'error') {
                            this.error = data.error || 'Download failed';
                            this.downloading = false;
                            this.eventSource.close();
                        }
                    };
                    this.eventSource.onerror = () => this.eventSource.close();
                },

                reset() {
                    this.url = '';
                    this.videoInfo = null;
                    this.format = 'mp4';
                    this.quality = 'best';
                    this.downloading = false;
                    this.completed = false;
                    this.queued = false;
                    this.progress = 0;
                    this.error = null;
                    this.downloadId = null;
                    this.downloadUrl = null;
                    if (this.eventSource) this.eventSource.close();
                }
            }
        }
    </script>
</body>
</html>
"""

if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host="0.0.0.0", port=8000)
