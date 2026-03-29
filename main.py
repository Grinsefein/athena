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
    <meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no, viewport-fit=cover">
    <title>Athena Pi</title>
    <script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js"></script>
    <script src="https://cdn.tailwindcss.com"></script>
    <script>
        tailwind.config = {
            darkMode: 'class',
            theme: {
                extend: {}
            }
        }
    </script>
    <link rel="icon" type="image/svg+xml" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'%3E%3Crect width='100' height='100' rx='20' fill='%236366f1'/%3E%3Cpath d='M35 30 L35 70 L75 50 Z' fill='white'/%3E%3C/svg%3E">
    <style>
        /* Base */
        *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
        html, body { height: 100%; }
        body { 
            font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
        }
        [x-cloak] { display: none !important; }
        
        /* Dotted Background Pattern */
        .bg-dots {
            background-color: #f8fafc;
            background-image: radial-gradient(#cbd5e1 1px, transparent 1px);
            background-size: 24px 24px;
        }
        .dark .bg-dots {
            background-color: #0f172a;
            background-image: radial-gradient(#334155 1px, transparent 1px);
        }
        
        /* Glass Morphism */
        .glass {
            background: rgba(255, 255, 255, 0.7);
            backdrop-filter: blur(12px);
            -webkit-backdrop-filter: blur(12px);
            border: 1px solid rgba(255, 255, 255, 0.3);
        }
        .dark .glass, .dark.glass {
            background: rgba(17, 24, 39, 0.85);
            border: 1px solid rgba(75, 85, 99, 0.4);
        }
        
        /* Card Transitions */
        .card-container {
            transition: all 0.5s cubic-bezier(0.4, 0, 0.2, 1);
        }
        
        /* Progress Bar Animation */
        .progress-fill {
            transition: width 0.3s ease;
        }
        
        /* Custom Scrollbar */
        ::-webkit-scrollbar { width: 6px; height: 6px; }
        ::-webkit-scrollbar-track { background: transparent; }
        ::-webkit-scrollbar-thumb { background: #cbd5e1; border-radius: 3px; }
        .dark ::-webkit-scrollbar-thumb { background: #475569; }
        
        /* Dropdown Animation */
        .dropdown-menu {
            animation: dropdownIn 0.15s ease-out;
        }
        @keyframes dropdownIn {
            from { opacity: 0; transform: translateY(-8px) scale(0.98); }
            to { opacity: 1; transform: translateY(0) scale(1); }
        }
        
        /* 16:9 Thumbnail */
        .thumb-container {
            position: relative;
            padding-bottom: 56.25%; /* 16:9 */
            height: 0;
            overflow: hidden;
            border-radius: 12px;
        }
        .thumb-container img {
            position: absolute;
            top: 0; left: 0;
            width: 100%; height: 100%;
            object-fit: cover;
        }
        .duration-badge {
            position: absolute;
            bottom: 8px;
            right: 8px;
            background: rgba(0, 0, 0, 0.8);
            color: white;
            padding: 2px 8px;
            border-radius: 4px;
            font-size: 12px;
            font-weight: 600;
        }
        
        /* Mobile Optimizations */
        @media (max-width: 768px) {
            .split-layout { flex-direction: column !important; }
            .left-pane { width: 100% !important; }
            .right-pane { width: 100% !important; }
            .thumb-container { padding-bottom: 56.25%; }
        }
        
        /* Touch Targets */
        @media (pointer: coarse) {
            .touch-target { min-height: 48px; }
        }
    </style>
</head>
<body x-data="athenaApp()" x-init="init()" 
      x-bind:class="darkMode ? 'dark bg-gray-900' : 'bg-gray-100 bg-dots'"
      class="min-h-screen flex items-center justify-center p-4 transition-colors duration-300">
    
    <!-- Theme Toggle -->
    <button @click="toggleTheme()" class="fixed top-4 right-4 z-50 p-3 rounded-xl glass shadow-lg hover:scale-105 transition-transform touch-target">
        <svg x-show="!darkMode" class="w-5 h-5 text-indigo-600" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M20.354 15.354A9 9 0 018.646 3.646 9.003 9.003 0 0012 21a9.003 9.003 0 008.354-5.646z"/>
        </svg>
        <svg x-show="darkMode" class="w-5 h-5 text-amber-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 3v1m0 16v1m9-9h-1M4 12H3m15.364 6.364l-.707-.707M6.343 6.343l-.707-.707m12.728 0l-.707.707M6.343 17.657l-.707.707M16 12a4 4 0 11-8 0 4 4 0 018 0z"/>
        </svg>
    </button>

    <!-- Main Card -->
    <div class="card-container glass rounded-3xl shadow-2xl overflow-hidden w-full max-w-md" 
         :class="{ 'max-w-4xl': videoInfo, 'md:max-w-4xl': videoInfo }">
        
        <!-- Search Phase -->
        <div class="p-6 md:p-8">
            <!-- Header -->
            <div class="text-center mb-6">
                <div class="inline-flex items-center justify-center w-14 h-14 rounded-2xl bg-gradient-to-br from-indigo-500 to-purple-600 text-white mb-4 shadow-lg">
                    <svg class="w-7 h-7" viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>
                </div>
                <h1 class="text-2xl font-bold text-gray-900 dark:text-white mb-1">Athena</h1>
                <p class="text-sm text-gray-500 dark:text-gray-400">YouTube Downloader</p>
            </div>

            <!-- URL Input -->
            <div class="relative mb-4">
                <input 
                    type="url" 
                    x-model="url"
                    @keydown.enter="analyze()"
                    placeholder="Paste YouTube URL..."
                    :disabled="loading"
                    class="w-full pl-4 pr-14 py-4 rounded-xl bg-gray-100/80 dark:bg-gray-800/80 border border-gray-200 dark:border-gray-700 focus:ring-2 focus:ring-indigo-500 focus:border-transparent outline-none text-gray-800 dark:text-gray-100 text-base touch-target transition-all"
                >
                <button 
                    @click="analyze()"
                    :disabled="!url || loading"
                    class="absolute right-2 top-1/2 -translate-y-1/2 p-2.5 rounded-lg bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 disabled:cursor-not-allowed text-white transition-colors"
                >
                    <svg x-show="loading" class="animate-spin w-5 h-5" fill="none" viewBox="0 0 24 24">
                        <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                        <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                    </svg>
                    <svg x-show="!loading" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M14 5l7 7m0 0l-7 7m7-7H3"/>
                    </svg>
                </button>
            </div>

            <!-- Error -->
            <div x-show="error" x-transition class="mb-4 p-3 rounded-lg bg-red-100 dark:bg-red-900/30 text-red-600 dark:text-red-400 text-sm text-center" x-text="error"></div>
        </div>

        <!-- Video Info & Download Panel -->
        <div x-show="videoInfo" x-transition:enter="transition ease-out duration-500" x-transition:enter-start="opacity-0" x-transition:enter-end="opacity-100" class="border-t border-gray-200 dark:border-gray-700" x-cloak>
            <div class="split-layout flex flex-col md:flex-row">
                
                <!-- Left Pane: Video Info (40% on desktop) -->
                <div class="left-pane w-full md:w-[40%] p-6 md:p-8 bg-gray-50/50 dark:bg-gray-900/50">
                    <!-- 16:9 Thumbnail with Duration -->
                    <div class="thumb-container mb-4 shadow-lg">
                        <img :src="videoInfo?.thumbnail" alt="Video thumbnail">
                        <div class="duration-badge" x-text="videoInfo?.duration || ''"></div>
                    </div>
                    
                    <!-- Video Details -->
                    <h3 x-text="videoInfo?.title" class="font-bold text-gray-900 dark:text-white text-lg leading-snug mb-2 line-clamp-2"></h3>
                    <p x-text="videoInfo?.author" class="text-gray-500 dark:text-gray-400 text-sm"></p>
                </div>

                <!-- Right Pane: Download Settings (60% on desktop) -->
                <div class="right-pane w-full md:w-[60%] p-6 md:p-8 flex flex-col justify-center">
                    
                    <!-- Format & Quality Dropdowns -->
                    <div class="grid grid-cols-2 gap-4 mb-6">
                        <!-- Format Dropdown -->
                        <div class="relative" x-data="{ open: false }" @click.away="open = false">
                            <label class="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1.5 uppercase tracking-wider">Format</label>
                            <button @click="open = !open" 
                                    class="w-full flex items-center justify-between px-4 py-3 rounded-xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 hover:border-indigo-400 dark:hover:border-indigo-500 transition-colors text-left touch-target">
                                <span class="font-medium text-gray-800 dark:text-gray-200" x-text="format === 'mp4' ? 'Video (MP4)' : format === 'mp3' ? 'Audio (MP3)' : 'Video (WebM)'"></span>
                                <svg class="w-4 h-4 text-gray-400 transition-transform" :class="{ 'rotate-180': open }" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"/>
                                </svg>
                            </button>
                            <div x-show="open" class="dropdown-menu absolute z-50 w-full mt-1 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl shadow-xl overflow-hidden">
                                <div @click="format = 'mp4'; open = false" class="px-4 py-3 hover:bg-gray-100 dark:hover:bg-gray-700 cursor-pointer flex items-center gap-3" :class="{ 'bg-indigo-50 dark:bg-indigo-900/30 text-indigo-600 dark:text-indigo-400': format === 'mp4' }">
                                    <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 10l4.553-2.276A1 1 0 0121 8.618v6.764a1 1 0 01-1.447.894L15 14M5 18h8a2 2 0 002-2V8a2 2 0 00-2-2H5a2 2 0 00-2 2v8a2 2 0 002 2z"/></svg>
                                    <span>Video (MP4)</span>
                                </div>
                                <div @click="format = 'mp3'; open = false" class="px-4 py-3 hover:bg-gray-100 dark:hover:bg-gray-700 cursor-pointer flex items-center gap-3" :class="{ 'bg-indigo-50 dark:bg-indigo-900/30 text-indigo-600 dark:text-indigo-400': format === 'mp3' }">
                                    <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 19V6l12-3v13M9 19c0 1.105-1.343 2-3 2s-3-.895-3-2 1.343-2 3-2 3 .895 3 2zm12-3c0 1.105-1.343 2-3 2s-3-.895-3-2 1.343-2 3-2 3 .895 3 2zM9 10l12-3"/></svg>
                                    <span>Audio (MP3)</span>
                                </div>
                                <div @click="format = 'webm'; open = false" class="px-4 py-3 hover:bg-gray-100 dark:hover:bg-gray-700 cursor-pointer flex items-center gap-3" :class="{ 'bg-indigo-50 dark:bg-indigo-900/30 text-indigo-600 dark:text-indigo-400': format === 'webm' }">
                                    <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 10l4.553-2.276A1 1 0 0121 8.618v6.764a1 1 0 01-1.447.894L15 14M5 18h8a2 2 0 002-2V8a2 2 0 00-2-2H5a2 2 0 00-2 2v8a2 2 0 002 2z"/></svg>
                                    <span>Video (WebM)</span>
                                </div>
                            </div>
                        </div>

                        <!-- Quality Dropdown -->
                        <div class="relative" x-data="{ open: false }" @click.away="open = false">
                            <label class="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1.5 uppercase tracking-wider">Quality</label>
                            <button @click="open = !open" 
                                    class="w-full flex items-center justify-between px-4 py-3 rounded-xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 hover:border-indigo-400 dark:hover:border-indigo-500 transition-colors text-left touch-target">
                                <span class="font-medium text-gray-800 dark:text-gray-200" x-text="quality === 'best' ? 'Best Quality' : quality"></span>
                                <svg class="w-4 h-4 text-gray-400 transition-transform" :class="{ 'rotate-180': open }" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"/>
                                </svg>
                            </button>
                            <div x-show="open" class="dropdown-menu absolute z-50 w-full mt-1 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl shadow-xl overflow-hidden max-h-60 overflow-y-auto">
                                <div @click="quality = 'best'; open = false" class="px-4 py-3 hover:bg-gray-100 dark:hover:bg-gray-700 cursor-pointer" :class="{ 'bg-indigo-50 dark:bg-indigo-900/30 text-indigo-600 dark:text-indigo-400': quality === 'best' }">Best Quality</div>
                                <template x-for="fmt in videoInfo?.formats || []" :key="fmt.quality">
                                    <div @click="quality = fmt.quality; open = false" class="px-4 py-3 hover:bg-gray-100 dark:hover:bg-gray-700 cursor-pointer" :class="{ 'bg-indigo-50 dark:bg-indigo-900/30 text-indigo-600 dark:text-indigo-400': quality === fmt.quality }" x-text="fmt.label"></div>
                                </template>
                            </div>
                        </div>
                    </div>

                    <!-- Download Button with Progress -->
                    <button 
                        @click="download()"
                        :disabled="downloading || completed"
                        class="relative w-full py-4 rounded-xl font-semibold text-white overflow-hidden touch-target transition-all hover:shadow-lg hover:scale-[1.02] active:scale-[0.98]"
                        :class="(downloading || completed) ? 'bg-indigo-400 cursor-not-allowed' : 'bg-gradient-to-r from-indigo-600 to-purple-600 hover:from-indigo-500 hover:to-purple-500'"
                    >
                        <!-- Progress Fill -->
                        <div x-show="downloading && !queued" class="progress-fill absolute inset-0 bg-indigo-700/50" :style="`width: ${progress}%`"></div>
                        <div x-show="queued" class="absolute inset-0 bg-amber-500/50 animate-pulse"></div>
                        
                        <!-- Content -->
                        <span class="relative flex items-center justify-center gap-2">
                            <svg x-show="downloading" class="animate-spin w-5 h-5" fill="none" viewBox="0 0 24 24">
                                <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                                <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                            </svg>
                            <svg x-show="!downloading && !completed" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4"/>
                            </svg>
                            <svg x-show="completed" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 13l4 4L19 7"/>
                            </svg>
                            <span x-text="downloading ? (queued ? 'Queued...' : `${progress}%`) : (completed ? 'Download Complete!' : 'Download')"></span>
                        </span>
                    </button>

                    <!-- Action Buttons -->
                    <div x-show="completed" x-transition class="flex gap-3 mt-4">
                        <a :href="downloadUrl" download class="flex-1 py-3 bg-emerald-500 hover:bg-emerald-600 text-white text-center rounded-xl font-semibold transition-colors touch-target flex items-center justify-center gap-2">
                            <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4"/>
                            </svg>
                            Save File
                        </a>
                        <button @click="reset()" class="px-6 py-3 rounded-xl bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-300 font-semibold hover:bg-gray-300 dark:hover:bg-gray-600 transition-colors touch-target">
                            New
                        </button>
                    </div>
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
                    if (this.darkMode) {
                        document.documentElement.classList.add('dark');
                    }
                },

                toggleTheme() {
                    this.darkMode = !this.darkMode;
                    localStorage.setItem('darkMode', this.darkMode);
                    if (this.darkMode) {
                        document.documentElement.classList.add('dark');
                    } else {
                        document.documentElement.classList.remove('dark');
                    }
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
