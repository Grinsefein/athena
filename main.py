from fastapi import FastAPI, HTTPException, BackgroundTasks
from fastapi.responses import StreamingResponse, FileResponse, HTMLResponse
from fastapi.staticfiles import StaticFiles
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel, HttpUrl
import yt_dlp
import asyncio
import json
import os
import uuid
from pathlib import Path
from typing import Optional
import logging

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

app = FastAPI(title="Athena Pi", version="1.0.0")

# CORS for local development
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# Configuration
DOWNLOAD_DIR = Path(os.getenv("DOWNLOAD_DIR", "./downloads"))
DOWNLOAD_DIR.mkdir(exist_ok=True)
MAX_FILE_AGE_HOURS = 24

# Active downloads storage (in-memory, lightweight)
active_downloads = {}


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
        "status": "processing",
        "progress": 0,
        "file_path": None,
        "file_name": None,
        "error": None
    }
    
    # Start download in background
    background_tasks.add_task(
        download_video_task,
        download_id,
        str(request.url),
        request.format,
        request.quality
    )
    
    return {
        "success": True,
        "data": {
            "downloadId": download_id,
            "status": "processing"
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


# Cleanup old files periodically
@app.on_event("startup")
async def startup_event():
    """Clean old downloads on startup"""
    cleanup_old_files()


def cleanup_old_files():
    """Remove files older than MAX_FILE_AGE_HOURS"""
    try:
        current_time = asyncio.get_event_loop().time()
        for file_path in DOWNLOAD_DIR.iterdir():
            if file_path.is_file():
                file_age_hours = (current_time - file_path.stat().st_mtime) / 3600
                if file_age_hours > MAX_FILE_AGE_HOURS:
                    file_path.unlink()
                    logger.info(f"Cleaned up old file: {file_path}")
    except Exception as e:
        logger.error(f"Cleanup error: {e}")


# Alpine.js Frontend (embedded for single-file deployment)
INDEX_HTML = """
<!DOCTYPE html>
<html lang="en" class="h-full">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=5">
    <title>Athena Pi - Video Downloader</title>
    <script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.x.x/dist/cdn.min.js"></script>
    <script src="https://cdn.tailwindcss.com"></script>
    <link rel="icon" type="image/svg+xml" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'%3E%3Crect width='100' height='100' rx='20' fill='%23dc2626'/%3E%3Cpath d='M35 30 L35 70 L75 50 Z' fill='white'/%3E%3C/svg%3E">
    <style>
        [x-cloak] { display: none !important; }
        .progress-bar { transition: width 0.3s ease; }
    </style>
</head>
<body class="h-full bg-gradient-to-br from-gray-50 to-gray-100 dark:from-gray-900 dark:to-gray-800"
      x-data="athenaApp()" x-init="init()">
    
    <div class="min-h-full flex flex-col">
        <!-- Header -->
        <header class="sticky top-0 z-50 bg-white/80 dark:bg-gray-900/80 backdrop-blur border-b">
            <div class="max-w-3xl mx-auto px-4 h-14 flex items-center justify-between">
                <div class="flex items-center gap-2">
                    <div class="w-8 h-8 bg-red-600 rounded-lg flex items-center justify-center">
                        <svg class="w-5 h-5 text-white" viewBox="0 0 24 24" fill="currentColor">
                            <path d="M8 5v14l11-7z"/>
                        </svg>
                    </div>
                    <span class="font-semibold text-lg">Athena Pi</span>
                </div>
                <button @click="toggleTheme()" class="p-2 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-800">
                    <svg x-show="!darkMode" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M20.354 15.354A9 9 0 018.646 3.646 9.003 9.003 0 0012 21a9.003 9.003 0 008.354-5.646z"/>
                    </svg>
                    <svg x-show="darkMode" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 3v1m0 16v1m9-9h-1M4 12H3m15.364 6.364l-.707-.707M6.343 6.343l-.707-.707m12.728 0l-.707.707M6.343 17.657l-.707.707M16 12a4 4 0 11-8 0 4 4 0 018 0z"/>
                    </svg>
                </button>
            </div>
        </header>

        <!-- Main Content -->
        <main class="flex-1 flex flex-col items-center justify-center p-4">
            <div class="w-full max-w-xl">
                <!-- Title -->
                <div class="text-center mb-6">
                    <h1 class="text-2xl sm:text-3xl font-bold mb-2">YouTube Downloader</h1>
                    <p class="text-gray-600 dark:text-gray-400">Download videos on your Pi</p>
                </div>

                <!-- Input Card -->
                <div class="bg-white dark:bg-gray-800 rounded-xl shadow-lg border p-4 sm:p-6">
                    <!-- URL Input -->
                    <div class="flex flex-col gap-3 mb-4">
                        <div class="relative">
                            <input 
                                type="url" 
                                x-model="url"
                                @keydown.enter="analyze()"
                                placeholder="Paste YouTube URL..."
                                class="w-full pl-10 pr-4 py-3 rounded-lg border bg-gray-50 dark:bg-gray-700 focus:ring-2 focus:ring-blue-500 outline-none"
                                :disabled="loading"
                            >
                            <svg class="absolute left-3 top-3.5 w-5 h-5 text-gray-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M13.828 10.172a4 4 0 00-5.656 0l-4 4a4 4 0 105.656 5.656l1.102-1.101m-.758-4.899a4 4 0 005.656 0l4-4a4 4 0 00-5.656-5.656l-1.1 1.1"/>
                            </svg>
                        </div>
                        <button 
                            @click="analyze()"
                            :disabled="!url || loading"
                            class="py-3 px-6 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-400 text-white font-medium rounded-lg transition flex items-center justify-center gap-2"
                        >
                            <svg x-show="loading" class="animate-spin w-5 h-5" fill="none" viewBox="0 0 24 24">
                                <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                                <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"/>
                            </svg>
                            <svg x-show="!loading" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M14.752 11.168l-3.197-2.132A1 1 0 0010 9.87v4.263a1 1 0 001.555.832l3.197-2.132a1 1 0 000-1.664z"/>
                                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 12a9 9 0 11-18 0 9 9 0 0118 0z"/>
                            </svg>
                            <span x-text="loading ? 'Analyzing...' : 'Analyze'"></span>
                        </button>
                    </div>

                    <!-- Error -->
                    <div x-show="error" x-transition class="mb-4 p-3 bg-red-50 dark:bg-red-900/30 border border-red-200 rounded-lg flex items-center gap-2 text-red-700 dark:text-red-400">
                        <svg class="w-5 h-5 flex-shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z"/>
                        </svg>
                        <span x-text="error" class="text-sm"></span>
                        <button @click="error = null" class="ml-auto p-1 hover:bg-red-100 rounded">
                            <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18L18 6M6 6l12 12"/>
                            </svg>
                        </button>
                    </div>

                    <!-- Video Info -->
                    <div x-show="videoInfo" x-transition class="mb-4">
                        <div class="flex gap-3 p-3 bg-gray-50 dark:bg-gray-700/50 rounded-lg">
                            <img :src="videoInfo?.thumbnail" class="w-24 h-16 object-cover rounded bg-gray-200 flex-shrink-0">
                            <div class="min-w-0">
                                <h3 x-text="videoInfo?.title" class="font-medium text-sm line-clamp-2 mb-1"></h3>
                                <p x-text="videoInfo?.author" class="text-xs text-gray-500"></p>
                                <span class="inline-flex items-center gap-1 mt-1 px-2 py-0.5 bg-green-100 text-green-700 text-xs rounded-full">
                                    <svg class="w-3 h-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 13l4 4L19 7"/>
                                    </svg>
                                    Ready
                                </span>
                            </div>
                        </div>
                    </div>

                    <!-- Format & Quality Selectors -->
                    <div x-show="videoInfo" x-transition class="grid grid-cols-2 gap-3 mb-4">
                        <div>
                            <label class="text-sm font-medium mb-1 block">Format</label>
                            <select x-model="format" class="w-full p-2.5 rounded-lg border bg-gray-50 dark:bg-gray-700">
                                <option value="mp4">MP4 Video</option>
                                <option value="mp3">MP3 Audio</option>
                                <option value="webm">WebM Video</option>
                            </select>
                        </div>
                        <div>
                            <label class="text-sm font-medium mb-1 block">Quality</label>
                            <select x-model="quality" class="w-full p-2.5 rounded-lg border bg-gray-50 dark:bg-gray-700">
                                <option value="best">Best Available</option>
                                <template x-for="fmt in videoInfo?.formats || []">
                                    <option :value="fmt.quality" x-text="fmt.label"></option>
                                </template>
                            </select>
                        </div>
                    </div>

                    <!-- Download Button -->
                    <button 
                        x-show="videoInfo"
                        @click="download()"
                        :disabled="downloading || completed"
                        class="w-full py-4 bg-gradient-to-r from-blue-600 to-indigo-600 hover:from-blue-700 hover:to-indigo-700 disabled:from-gray-400 disabled:to-gray-500 text-white font-medium rounded-lg transition flex items-center justify-center gap-2"
                    >
                        <svg x-show="downloading" class="animate-spin w-5 h-5" fill="none" viewBox="0 0 24 24">
                            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                            <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"/>
                        </svg>
                        <svg x-show="!downloading && !completed" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4"/>
                        </svg>
                        <svg x-show="completed" class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 13l4 4L19 7"/>
                        </svg>
                        <span x-text="downloading ? `Downloading... ${progress}%` : (completed ? 'Complete!' : `Download ${format.toUpperCase()}`)"></span>
                    </button>

                    <!-- Progress Bar -->
                    <div x-show="downloading" x-transition class="mt-3">
                        <div class="h-2 bg-gray-200 dark:bg-gray-700 rounded-full overflow-hidden">
                            <div class="h-full bg-gradient-to-r from-blue-500 to-indigo-500 progress-bar" :style="`width: ${progress}%`"></div>
                        </div>
                        <p class="text-xs text-gray-500 text-center mt-2">Don't close this tab</p>
                    </div>

                    <!-- Download Complete -->
                    <div x-show="completed" x-transition class="mt-3 p-3 bg-green-50 dark:bg-green-900/30 border border-green-200 rounded-lg">
                        <div class="flex items-center gap-2 mb-3">
                            <div class="w-8 h-8 bg-green-100 rounded-full flex items-center justify-center">
                                <svg class="w-4 h-4 text-green-600" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 13l4 4L19 7"/>
                                </svg>
                            </div>
                            <span class="font-medium text-green-800 dark:text-green-400">Ready!</span>
                        </div>
                        <div class="flex gap-2">
                            <a :href="downloadUrl" download class="flex-1 py-2.5 bg-green-600 hover:bg-green-700 text-white text-center rounded-lg font-medium">
                                Save File
                            </a>
                            <button @click="reset()" class="px-4 py-2.5 border rounded-lg hover:bg-gray-50">
                                New
                            </button>
                        </div>
                    </div>
                </div>
            </div>
        </main>
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
                progress: 0,
                error: null,
                videoInfo: null,
                downloadId: null,
                downloadUrl: null,
                darkMode: false,
                eventSource: null,

                init() {
                    this.darkMode = localStorage.getItem('darkMode') === 'true';
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
                        
                        if (!data.success) {
                            throw new Error('Failed to analyze');
                        }

                        this.videoInfo = data.data;
                    } catch (e) {
                        this.error = 'Failed to analyze video. Check the URL.';
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
                        
                        if (!data.success) {
                            throw new Error('Failed to start download');
                        }

                        this.downloadId = data.data.downloadId;
                        this.connectSSE();
                    } catch (e) {
                        this.error = 'Download failed to start.';
                        this.downloading = false;
                    }
                },

                connectSSE() {
                    if (this.eventSource) {
                        this.eventSource.close();
                    }

                    this.eventSource = new EventSource(`/api/progress/${this.downloadId}`);
                    
                    this.eventSource.onmessage = (event) => {
                        const data = JSON.parse(event.data);
                        
                        if (data.error) {
                            this.error = data.error;
                            this.downloading = false;
                            this.eventSource.close();
                            return;
                        }

                        this.progress = data.progress || 0;

                        if (data.status === 'completed') {
                            this.downloading = false;
                            this.completed = true;
                            this.downloadUrl = data.downloadUrl;
                            this.eventSource.close();
                        } else if (data.status === 'error') {
                            this.error = data.error || 'Download failed';
                            this.downloading = false;
                            this.eventSource.close();
                        }
                    };

                    this.eventSource.onerror = () => {
                        this.eventSource.close();
                    };
                },

                reset() {
                    this.url = '';
                    this.videoInfo = null;
                    this.format = 'mp4';
                    this.quality = 'best';
                    this.downloading = false;
                    this.completed = false;
                    this.progress = 0;
                    this.error = null;
                    this.downloadId = null;
                    this.downloadUrl = null;
                    if (this.eventSource) {
                        this.eventSource.close();
                    }
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
