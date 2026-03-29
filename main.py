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
<html lang="en" class="h-full">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=5">
    <title>Athena Pi</title>
    <script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js"></script>
    <script src="https://cdn.tailwindcss.com"></script>
    <link rel="icon" type="image/svg+xml" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'%3E%3Crect width='100' height='100' rx='20' fill='%236366f1'/%3E%3Cpath d='M35 30 L35 70 L75 50 Z' fill='white'/%3E%3C/svg%3E">
    <link href="https://fonts.googleapis.com/css2?family=Outfit:wght@300;400;500;600;700&display=swap" rel="stylesheet">
    <style>
        body { font-family: 'Outfit', sans-serif; }
        [x-cloak] { display: none !important; }
        .bg-animated { background-size: 200% 200%; animation: gradient 15s ease infinite; }
        @keyframes gradient { 0% { background-position: 0% 50%; } 50% { background-position: 100% 50%; } 100% { background-position: 0% 50%; } }
        .glass-card { backdrop-filter: blur(24px); -webkit-backdrop-filter: blur(24px); }
        .progress-bar { transition: width 0.4s cubic-bezier(0.4, 0, 0.2, 1); }
    </style>
</head>
<body class="h-full bg-animated bg-gradient-to-br from-indigo-50 via-purple-50 to-pink-50 dark:from-slate-900 dark:via-purple-900/20 dark:to-slate-900"
      x-data="athenaApp()" x-init="init()">
    
    <!-- Theme Toggle -->
    <button @click="toggleTheme()" class="fixed top-4 right-4 z-50 p-3.5 rounded-2xl glass-card bg-white/40 dark:bg-black/30 border border-white/40 dark:border-white/10 shadow-xl hover:scale-110 active:scale-95 transition-all duration-300">
        <svg x-show="!darkMode" class="w-5 h-5 text-indigo-600" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M20.354 15.354A9 9 0 018.646 3.646 9.003 9.003 0 0012 21a9.003 9.003 0 008.354-5.646z"/></svg>
        <svg x-show="darkMode" class="w-5 h-5 text-yellow-300" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 3v1m0 16v1m9-9h-1M4 12H3m15.364 6.364l-.707-.707M6.343 6.343l-.707-.707m12.728 0l-.707.707M6.343 17.657l-.707.707M16 12a4 4 0 11-8 0 4 4 0 018 0z"/></svg>
    </button>

    <main class="min-h-full flex items-center justify-center p-4 sm:p-8">
        <div class="w-full max-w-lg">
            
            <!-- Glass Wrapper -->
            <div class="glass-card bg-white/60 dark:bg-slate-800/60 rounded-[2rem] shadow-2xl border border-white/50 dark:border-slate-700/50 p-6 sm:p-8 relative overflow-hidden transition-all duration-500">
                <!-- Decorative Blurs -->
                <div class="absolute -top-24 -right-24 w-48 h-48 bg-purple-500/30 rounded-full blur-3xl pointer-events-none"></div>
                <div class="absolute -bottom-24 -left-24 w-48 h-48 bg-indigo-500/20 rounded-full blur-3xl pointer-events-none"></div>

                <div class="relative z-10">
                    <!-- Minimal Header -->
                    <div class="text-center mb-8">
                        <div class="inline-flex items-center justify-center w-14 h-14 rounded-2xl bg-gradient-to-tr from-indigo-500 to-purple-500 text-white mb-5 shadow-lg shadow-indigo-500/30 ring-4 ring-white/50 dark:ring-slate-800/50">
                            <svg class="w-7 h-7" viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>
                        </div>
                        <h1 class="text-3xl sm:text-4xl font-bold bg-clip-text text-transparent bg-gradient-to-r from-gray-900 to-gray-600 dark:from-white dark:to-gray-300 tracking-tight">Athena</h1>
                        <p class="text-sm font-medium text-gray-500 dark:text-gray-400 mt-1">Pi Downloader</p>
                    </div>

                    <!-- Input Area -->
                    <div class="space-y-4">
                        <div class="relative group">
                            <input 
                                type="url" 
                                x-model="url"
                                @keydown.enter="analyze()"
                                placeholder="Paste video link here..."
                                class="w-full pl-5 pr-14 py-4 rounded-2xl bg-white/50 dark:bg-slate-900/50 border border-gray-200/50 dark:border-slate-700/50 focus:ring-2 focus:ring-indigo-500/50 focus:border-transparent outline-none transition-all duration-300 text-gray-800 dark:text-gray-100 placeholder-gray-400 font-medium"
                                :disabled="loading"
                            >
                            <button 
                                @click="analyze()"
                                :disabled="!url || loading"
                                class="absolute right-2 top-2 p-2.5 rounded-xl bg-indigo-600 hover:bg-indigo-500 active:scale-95 disabled:opacity-50 disabled:active:scale-100 text-white transition-all duration-300 shadow-md"
                            >
                                <svg x-show="loading" class="animate-spin w-5 h-5" fill="none" viewBox="0 0 24 24">
                                    <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                                    <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"/>
                                </svg>
                                <svg x-show="!loading" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M14 5l7 7m0 0l-7 7m7-7H3"/>
                                </svg>
                            </button>
                        </div>

                        <!-- Error Toast -->
                        <div x-show="error" x-transition class="text-center text-sm font-bold text-red-500 bg-red-500/10 py-3 px-4 rounded-xl border border-red-500/20" x-text="error"></div>

                        <!-- Video Info & Options -->
                        <div x-show="videoInfo" x-transition:enter="transition ease-out duration-500 delay-100" x-transition:enter-start="opacity-0 translate-y-4" x-transition:enter-end="opacity-100 translate-y-0" class="space-y-4 mt-6" x-cloak>
                            
                            <!-- Media Card -->
                            <div class="flex items-center gap-4 p-3 rounded-2xl bg-white/40 dark:bg-slate-900/40 border border-white/40 dark:border-slate-700/30">
                                <div class="relative w-20 h-20 rounded-xl overflow-hidden shadow-sm flex-shrink-0 bg-gray-200 dark:bg-gray-800">
                                    <img :src="videoInfo?.thumbnail" class="absolute inset-0 w-full h-full object-cover">
                                </div>
                                <div class="min-w-0 pr-2">
                                    <h3 x-text="videoInfo?.title" class="font-bold text-gray-800 dark:text-gray-100 truncate text-sm sm:text-base"></h3>
                                    <p x-text="videoInfo?.author" class="text-xs sm:text-sm font-medium text-gray-500 dark:text-gray-400 truncate mt-0.5"></p>
                                </div>
                            </div>

                            <!-- Options Grid -->
                            <div class="grid grid-cols-2 gap-3">
                                <select x-model="format" class="w-full p-3.5 rounded-xl bg-white/50 dark:bg-slate-900/50 border border-gray-200/50 dark:border-slate-700/50 text-sm font-semibold text-gray-700 dark:text-gray-200 outline-none focus:ring-2 focus:ring-indigo-500/50 appearance-none cursor-pointer hover:bg-white/70 dark:hover:bg-slate-900/70 transition-colors">
                                    <option value="mp4">Video (MP4)</option>
                                    <option value="mp3">Audio (MP3)</option>
                                    <option value="webm">Video (WebM)</option>
                                </select>
                                <select x-model="quality" class="w-full p-3.5 rounded-xl bg-white/50 dark:bg-slate-900/50 border border-gray-200/50 dark:border-slate-700/50 text-sm font-semibold text-gray-700 dark:text-gray-200 outline-none focus:ring-2 focus:ring-indigo-500/50 appearance-none cursor-pointer hover:bg-white/70 dark:hover:bg-slate-900/70 transition-colors">
                                    <option value="best">Highest</option>
                                    <template x-for="fmt in videoInfo?.formats || []">
                                        <option :value="fmt.quality" x-text="fmt.label"></option>
                                    </template>
                                </select>
                            </div>

                            <!-- Action Button -->
                            <div class="pt-2">
                                <button 
                                    @click="download()"
                                    :disabled="downloading || completed"
                                    class="relative overflow-hidden w-full py-4 rounded-2xl font-bold text-white transition-all duration-300 hover:scale-[1.02] active:scale-95 disabled:hover:scale-100 group"
                                    :class="(downloading || completed) ? 'bg-indigo-400/80 dark:bg-indigo-900/80 backdrop-blur-md cursor-not-allowed' : 'bg-gradient-to-r from-indigo-500 to-purple-600 shadow-xl shadow-indigo-500/30 hover:shadow-indigo-500/40'"
                                >
                                    <!-- Progress Layer -->
                                    <div x-show="downloading && !queued" class="absolute inset-0 bg-indigo-600/60 dark:bg-indigo-500/60 origin-left progress-bar rounded-2xl" :style="`width: ${progress}%`"></div>
                                    <div x-show="queued" class="absolute inset-0 bg-yellow-500/30 dark:bg-yellow-600/30 animate-pulse rounded-2xl"></div>
                                    
                                    <div class="relative flex items-center justify-center gap-2 z-10">
                                        <svg x-show="downloading" class="animate-spin w-5 h-5 text-white/90" fill="none" viewBox="0 0 24 24">
                                            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                                            <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"/>
                                        </svg>
                                        <svg x-show="!downloading && !completed" class="w-5 h-5 transition-transform group-hover:-translate-y-0.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2.5" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4"/>
                                        </svg>
                                        <svg x-show="completed" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2.5" d="M5 13l4 4L19 7"/>
                                        </svg>
                                        <span x-text="downloading ? (queued ? 'Queued (Waiting in line...)' : `Transferring • ${progress}%`) : (completed ? 'Operation Complete!' : 'Begin Download')"></span>
                                    </div>
                                </button>
                            </div>

                            <!-- Finished State Docs -->
                            <div x-show="completed" x-transition:enter="transition ease-out duration-500 delay-150" x-transition:enter-start="opacity-0 scale-95" x-transition:enter-end="opacity-100 scale-100" class="pt-2 flex gap-3" x-cloak>
                                <a :href="downloadUrl" download class="flex-1 py-3.5 bg-gradient-to-br from-green-400 to-emerald-600 text-white shadow-lg shadow-emerald-500/20 text-center rounded-xl font-bold hover:scale-[1.02] active:scale-95 transition-all outline-none">
                                    Save to Device
                                </a>
                                <button @click="reset()" class="flex-none px-6 py-3.5 rounded-xl bg-white/60 dark:bg-slate-900/60 border border-gray-200/50 dark:border-slate-700/50 text-gray-700 dark:text-gray-200 font-bold hover:scale-[1.02] active:scale-95 hover:bg-white dark:hover:bg-slate-800 transition-all outline-none">
                                    Next
                                </button>
                            </div>
                        </div>

                    </div>
                </div>
            </div>
        </div>
    </main>

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
                    this.updateTheme();
                },

                toggleTheme() {
                    this.darkMode = !this.darkMode;
                    localStorage.setItem('darkMode', this.darkMode);
                    this.updateTheme();
                },

                updateTheme() {
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
                        this.error = 'Unrecognized link. Check URL parameters.';
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
                            body: JSON.stringify({ 
                                url: this.url, 
                                format: this.format,
                                quality: this.quality 
                            })
                        });

                        const data = await response.json();
                        if (!data.success) throw new Error('Failed to initiate');
                        
                        this.downloadId = data.data.downloadId;
                        this.queued = (data.data.status === 'queued');
                        this.connectSSE();
                    } catch (e) {
                        this.error = 'Transfer failed to start.';
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

                        if (data.status === 'queued') {
                            this.queued = true;
                        } else if (data.status === 'processing') {
                            this.queued = false;
                        }

                        this.progress = data.progress || 0;

                        if (data.status === 'completed') {
                            this.downloading = false;
                            this.queued = false;
                            this.completed = true;
                            this.downloadUrl = data.downloadUrl;
                            this.eventSource.close();
                        } else if (data.status === 'error') {
                            this.error = data.error || 'Transfer failed';
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
