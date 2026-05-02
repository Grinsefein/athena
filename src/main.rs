use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::{sse::Event, Html, Json, Sse, Response, IntoResponse},
    routing::{get, post},
    Router,
};
use futures::stream::{Stream, StreamExt};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::{Mutex, Semaphore},
    time::{interval, sleep},
};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use uuid::Uuid;

// Static HTML frontend (embedded from Python version)
static INDEX_HTML: &str = include_str!("../frontend.html");

// Configuration - use system temp directory for temporary storage
static DOWNLOAD_DIR: Lazy<PathBuf> = Lazy::new(|| {
    std::env::var("DOWNLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("athena-downloads"))
});

// Short max age for temp files (1 hour)
const MAX_FILE_AGE_HOURS: f64 = 1.0;
const CLEANUP_INTERVAL_SECONDS: u64 = 600;

// Global semaphore for concurrent downloads
static DOWNLOAD_SEMAPHORE: Lazy<Arc<Semaphore>> = Lazy::new(|| {
    let max = std::env::var("MAX_CONCURRENT_DOWNLOADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    Arc::new(Semaphore::new(max))
});

// Pre-compiled regex for progress parsing
static PROGRESS_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\[download\]\s+(\d+\.?\d*)%").unwrap()
});

// App state
type SharedState = Arc<AppState>;

struct AppState {
    active_downloads: Mutex<HashMap<String, DownloadInfo>>,
}

#[derive(Clone, Debug)]
struct DownloadInfo {
    status: String,
    progress: f64,
    file_path: Option<PathBuf>,
    file_name: Option<String>,
    error: Option<String>,
    timestamp: f64,
}

// Request/Response models
#[derive(Deserialize)]
struct DownloadRequest {
    url: String,
    #[serde(default = "default_format")]
    format: String,
    #[serde(default = "default_quality")]
    quality: String,
}

fn default_format() -> String {
    String::from("mp4")
}
fn default_quality() -> String {
    String::from("best")
}

#[derive(Serialize)]
struct ApiResponse<T> {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct AnalyzeResponse {
    id: String,
    title: String,
    description: String,
    thumbnail: String,
    duration: String,
    author: String,
    formats: Vec<FormatInfo>,
}

#[derive(Serialize)]
struct FormatInfo {
    format: String,
    quality: String,
    label: String,
}

#[derive(Serialize)]
struct DownloadResponse {
    download_id: String,
    status: String,
}

#[derive(Serialize, Clone)]
struct ProgressUpdate {
    status: String,
    progress: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(true)
        .with_thread_ids(true)
        .with_line_number(true)
        .compact()
        .finish();
    
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set subscriber");

    // Ensure download directory exists
    fs::create_dir_all(&*DOWNLOAD_DIR)
        .await
        .expect("Failed to create download directory");

    info!("Download directory: {:?}", *DOWNLOAD_DIR);
    info!("Max concurrent downloads: {}", DOWNLOAD_SEMAPHORE.available_permits());

    // Check yt-dlp version and update if needed
    tokio::spawn(async {
        update_yt_dlp().await;
    });

    // Initialize state
    let state = Arc::new(AppState {
        active_downloads: Mutex::new(HashMap::new()),
    });

    // Start cleanup task
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        periodic_cleanup(cleanup_state).await;
    });

    // Start yt-dlp auto-update task (checks every 24 hours)
    tokio::spawn(async {
        periodic_yt_dlp_update().await;
    });

    // Build router
    let app = Router::new()
        .route("/", get(root))
        .route("/api/analyze", post(analyze_video))
        .route("/api/download", post(start_download))
        .route("/api/progress/:download_id", get(progress_stream))
        .route("/api/file/:download_id", get(download_file))
        .route("/api/ytdlp-version", get(get_ytdlp_version))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8000);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap();

    info!("Athena server listening on 0.0.0.0:{}", port);

    axum::serve(listener, app).await.unwrap();
}

async fn root() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn analyze_video(
    Json(request): Json<DownloadRequest>,
) -> Result<Json<ApiResponse<AnalyzeResponse>>, StatusCode> {
    info!("Analyzing video: {}", request.url);

    let output = Command::new("yt-dlp")
        .args([
            "--quiet",
            "--no-warnings",
            "--dump-json",
            "--no-download",
            &request.url,
        ])
        .output()
        .await
        .map_err(|e| {
            error!("Failed to execute yt-dlp: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("yt-dlp analyze failed: {}", stderr);
        return Ok(Json(ApiResponse {
            success: false,
            data: None,
            error: Some("Failed to analyze video".to_string()),
        }));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let info: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| {
            error!("Failed to parse yt-dlp output: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Format duration
    let duration_secs = info.get("duration")
        .and_then(|d| d.as_f64())
        .unwrap_or(0.0) as u64;
    let duration_mins = duration_secs / 60;
    let duration_rem = duration_secs % 60;
    let duration = format!("{}:{:02}", duration_mins, duration_rem);

    // Extract formats with pre-allocated collections
    let mut video_formats = Vec::with_capacity(4);
    let mut audio_formats = Vec::with_capacity(1);
    let mut seen_qualities = std::collections::HashSet::with_capacity(8);

    if let Some(formats) = info.get("formats").and_then(|f| f.as_array()) {
        for fmt in formats {
            let vcodec = fmt.get("vcodec").and_then(|v| v.as_str()).unwrap_or("none");
            let acodec = fmt.get("acodec").and_then(|a| a.as_str()).unwrap_or("none");
            let resolution = fmt.get("resolution").and_then(|r| r.as_str()).unwrap_or("unknown");
            let ext = fmt.get("ext").and_then(|e| e.as_str()).unwrap_or("unknown");

            if vcodec != "none" && resolution != "unknown" {
                // Use a stack buffer for the key to avoid heap allocation
                let key = format!("{}-{}", resolution, ext);
                if seen_qualities.insert(key) {
                    video_formats.push(FormatInfo {
                        format: String::from(ext),
                        quality: String::from(resolution),
                        label: format!("{} ({})", resolution, ext),
                    });
                    if video_formats.len() >= 4 {
                        break;
                    }
                }
            }

            // Only add audio format once
            if acodec != "none" && vcodec == "none" && audio_formats.is_empty() {
                audio_formats.push(FormatInfo {
                    format: String::from("mp3"),
                    quality: String::from("best"),
                    label: String::from("MP3 Audio (Best)"),
                });
            }
        }
    }

    // Combine formats efficiently
    let mut all_formats = Vec::with_capacity(video_formats.len() + audio_formats.len());
    all_formats.extend(video_formats);
    all_formats.extend(audio_formats);

    let description = info.get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect::<String>();

    let response = AnalyzeResponse {
        id: info.get("id").and_then(|i| i.as_str()).unwrap_or("unknown").to_string(),
        title: info.get("title").and_then(|t| t.as_str()).unwrap_or("Unknown").to_string(),
        description,
        thumbnail: info.get("thumbnail").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        duration,
        author: info.get("uploader").and_then(|u| u.as_str()).unwrap_or("Unknown").to_string(),
        formats: all_formats,
    };

    Ok(Json(ApiResponse {
        success: true,
        data: Some(response),
        error: None,
    }))
}

async fn get_ytdlp_version() -> Json<ApiResponse<serde_json::Value>> {
    let output = Command::new("yt-dlp")
        .args(["--version"])
        .output()
        .await;

    match output {
        Ok(result) if result.status.success() => {
            let version = String::from_utf8_lossy(&result.stdout).trim().to_string();
            Json(ApiResponse {
                success: true,
                data: Some(serde_json::json!({ "version": version })),
                error: None,
            })
        }
        _ => Json(ApiResponse {
            success: false,
            data: None,
            error: Some("Could not determine yt-dlp version".to_string()),
        }),
    }
}

async fn start_download(
    State(state): State<SharedState>,
    Json(request): Json<DownloadRequest>,
) -> Json<ApiResponse<DownloadResponse>> {
    let download_id = Uuid::new_v4().to_string()[..8].to_string();
    info!("Starting download {} for URL: {}", download_id, request.url);

    {
        let mut downloads = state.active_downloads.lock().await;
        downloads.insert(download_id.clone(), DownloadInfo {
            status: "queued".to_string(),
            progress: 0.0,
            file_path: None,
            file_name: None,
            error: None,
            timestamp: now_secs(),
        });
    }

    // Spawn background task
    let state_clone = state.clone();
    let download_id_clone = download_id.clone();
    tokio::spawn(async move {
        download_task(
            state_clone,
            download_id_clone,
            request.url,
            request.format,
            request.quality,
        )
        .await;
    });

    Json(ApiResponse {
        success: true,
        data: Some(DownloadResponse {
            download_id,
            status: "queued".to_string(),
        }),
        error: None,
    })
}

async fn download_task(
    state: SharedState,
    download_id: String,
    url: String,
    format_type: String,
    quality: String,
) {
    info!("Download {} waiting for semaphore...", download_id);
    
    let permit = DOWNLOAD_SEMAPHORE.acquire().await;
    
    {
        let mut downloads = state.active_downloads.lock().await;
        if let Some(info) = downloads.get_mut(&download_id) {
            info.status = "processing".to_string();
        }
    }
    
    info!("Download {} starting processing", download_id);

    let result = execute_download(&state, &download_id, &url, &format_type, &quality).await;

    if let Err(e) = result {
        error!("Download {} failed: {}", download_id, e);
        let mut downloads = state.active_downloads.lock().await;
        if let Some(info) = downloads.get_mut(&download_id) {
            info.status = "error".to_string();
            info.error = Some(e);
        }
    }

    drop(permit);
}

async fn execute_download(
    state: &SharedState,
    download_id: &str,
    url: &str,
    format_type: &str,
    quality: &str,
) -> Result<(), String> {
    // Get video info first to determine filename
    let info_output = Command::new("yt-dlp")
        .args(["--quiet", "--no-warnings", "--dump-json", "--no-download", url])
        .output()
        .await
        .map_err(|e| format!("Failed to get video info: {}", e))?;

    if !info_output.status.success() {
        return Err("Failed to analyze video".to_string());
    }

    let video_info: serde_json::Value = serde_json::from_slice(&info_output.stdout)
        .map_err(|e| format!("Failed to parse video info: {}", e))?;

    let title = video_info.get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("download");

    let ext = if format_type == "mp3" { "mp3" } else { format_type };
    let _file_name = format!("{}.{}", sanitize_filename(title), ext);

    // Build format string with minimal allocations
    let format_arg = if format_type == "mp3" {
        Cow::Borrowed("bestaudio/best")
    } else if quality == "best" {
        Cow::Owned(format!("best[ext={}]/best", format_type))
    } else {
        let height: String = quality.chars().filter(|c| c.is_ascii_digit()).collect();
        Cow::Owned(format!("best[height<={}][ext={}]/best[height<={}]/best", height, format_type, height))
    };

    // Use download_id in output template to reliably find the file later
    // yt-dlp will create: {title}-{id}.{ext} which we can match by the unique ID
    let output_template = DOWNLOAD_DIR.join(format!("%(title)s-[{}].%(ext)s", download_id));
    let output_template_str = output_template.to_str().ok_or("Invalid output path")?;

    // Pre-allocate vec to avoid reallocations
    let mut args = Vec::with_capacity(12);
    args.extend_from_slice(&[
        "--quiet",
        "--no-warnings",
        "--newline",
        "--progress",
        "-f", &format_arg,
        "-o", output_template_str,
    ]);

    if format_type == "mp3" {
        args.push("--extract-audio");
        args.push("--audio-format");
        args.push("mp3");
        args.push("--audio-quality");
        args.push("192K");
    }

    args.push(url);

    info!("Executing yt-dlp with args: {:?}", args);

    let mut child = Command::new("yt-dlp")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn yt-dlp: {}", e))?;

    let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    // Read progress lines
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(cap) = PROGRESS_REGEX.captures(&line) {
            if let Some(percent_match) = cap.get(1) {
                if let Ok(percent) = percent_match.as_str().parse::<f64>() {
                    let mut downloads = state.active_downloads.lock().await;
                    if let Some(info) = downloads.get_mut(download_id) {
                        info.progress = percent;
                    }
                }
            }
        }
    }

    // Also capture stderr for error reporting
    let stderr = child.stderr.take();
    
    let status = child.wait().await.map_err(|e| format!("Failed to wait for process: {}", e))?;

    if !status.success() {
        // Read stderr to get actual error message
        let mut error_msg = "yt-dlp process failed".to_string();
        if let Some(stderr) = stderr {
            let mut reader = BufReader::new(stderr);
            let mut buffer = String::new();
            if reader.read_to_string(&mut buffer).await.is_ok() && !buffer.is_empty() {
                error_msg = format!("yt-dlp error: {}", buffer.trim());
                error!("{}", error_msg);
            }
        }
        return Err(error_msg);
    }

    // Find the downloaded file by looking for files containing the download_id
    // yt-dlp creates: "{title}-[{download_id}].{ext}" based on our output template
    let mut downloaded_file: Option<PathBuf> = None;
    let id_marker = format!("[{}]", download_id);
    
    // Give yt-dlp a moment to finish writing and rename .part file
    sleep(Duration::from_millis(500)).await;
    
    match fs::read_dir(&*DOWNLOAD_DIR).await {
        Ok(mut entries) => {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    // Match files containing our unique download_id marker and having the right extension
                    if name.contains(&id_marker) && name.ends_with(&format!(".{}", ext)) {
                        info!("Found downloaded file: {}", name);
                        downloaded_file = Some(path);
                        break;
                    }
                }
            }
        }
        Err(e) => warn!("Failed to read download directory: {}", e),
    }

    let final_path = match downloaded_file {
        Some(path) => path,
        None => {
            error!("Could not find downloaded file with ID marker: {}", id_marker);
            return Err("Downloaded file not found".to_string());
        }
    };

    // Update state with exact final path
    let final_file_name = final_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&_file_name)
        .to_string();
        
    {
        let mut downloads = state.active_downloads.lock().await;
        if let Some(info) = downloads.get_mut(download_id) {
            info.status = "completed".to_string();
            info.progress = 100.0;
            info.file_path = Some(final_path.clone());
            info.file_name = Some(final_file_name);
        }
    }

    info!("Download {} completed: {:?}", download_id, final_path);
    Ok(())
}

async fn progress_stream(
    State(state): State<SharedState>,
    Path(download_id): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    // Create the SSE stream without cloning state
    let stream = async_stream::stream! {
        loop {
            let (status, progress, file_path, error) = {
                let downloads = state.active_downloads.lock().await;
                match downloads.get(&download_id) {
                    Some(info) => (
                        info.status.clone(),
                        info.progress,
                        info.file_path.clone(),
                        info.error.clone(),
                    ),
                    None => {
                        let json = serde_json::to_string(&ProgressUpdate {
                            status: String::from("error"),
                            progress: 0.0,
                            download_url: None,
                            error: Some(String::from("Download not found")),
                        }).unwrap_or_default();
                        yield Ok(Event::default().data(json));
                        break;
                    }
                }
            };

            let should_break = status == "completed" || status == "error";
            let download_url = if status == "completed" && file_path.is_some() {
                Some(format!("/api/file/{}", download_id))
            } else {
                None
            };

            let json = serde_json::to_string(&ProgressUpdate {
                status,
                progress,
                download_url,
                error,
            }).unwrap_or_default();
            yield Ok(Event::default().data(json));

            if should_break {
                break;
            }

            sleep(Duration::from_millis(500)).await;
        }
    };

    Sse::new(stream)
}

async fn download_file(
    State(state): State<SharedState>,
    Path(download_id): Path<String>,
) -> Response {
    let download_info = {
        let downloads = state.active_downloads.lock().await;
        downloads.get(&download_id).cloned()
    };

    let info = match download_info {
        Some(i) if i.status == "completed" => i,
        _ => {
            return (StatusCode::NOT_FOUND, "Download not found or not ready").into_response();
        }
    };

    let file_path = match info.file_path {
        Some(p) => p,
        None => {
            return (StatusCode::NOT_FOUND, "File not found").into_response();
        }
    };

    if !file_path.exists() {
        return (StatusCode::NOT_FOUND, "File not found on disk").into_response();
    }

    let file_name = info.file_name
        .map(|name| {
            // Remove the ID marker from the filename for a cleaner display
            // Convert "Title-[id].mp4" to "Title.mp4"
            let id_marker = format!("-[{}].", download_id);
            name.replace(&id_marker, ".")
        })
        .unwrap_or_else(|| download_id.clone());
    let file_size = match fs::metadata(&file_path).await {
        Ok(meta) => meta.len(),
        Err(e) => {
            error!("Failed to get file metadata: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let file_path_clone = file_path.clone();
    let download_id_clone = download_id.clone();

    match tokio::fs::File::open(&file_path).await {
        Ok(file) => {
            let stream = tokio_util::io::ReaderStream::new(file);
            // Wrap stream to delete file after streaming completes
            let wrapped_stream = stream.then(move |chunk| {
                let path = file_path_clone.clone();
                let _id = download_id_clone.clone();
                async move {
                    // If this is the last chunk (Err or Ok with empty), delete the file
                    if chunk.is_err() {
                        // Try to delete file on error
                        let _ = fs::remove_file(&path).await;
                    }
                    chunk
                }
            });
            let body = Body::from_stream(wrapped_stream);

            // Build response with proper header handling
            let mut response = (StatusCode::OK, body).into_response();
            let headers = response.headers_mut();
            headers.insert("content-type", "application/octet-stream".parse().unwrap());
            headers.insert("content-disposition", format!("attachment; filename=\"{}\"", file_name).parse().unwrap());
            headers.insert("content-length", file_size.to_string().parse().unwrap());

            // Delete file after response is sent
            tokio::spawn(async move {
                // Give a small delay to ensure streaming starts, then mark for cleanup
                sleep(Duration::from_secs(2)).await;
                if let Err(e) = fs::remove_file(&file_path).await {
                    warn!("Failed to remove temp file {:?}: {}", file_path, e);
                } else {
                    info!("Removed temp file for download {}: {:?}", download_id, file_path);
                }
                // Remove from memory map too
                let mut downloads = state.active_downloads.lock().await;
                downloads.remove(&download_id);
            });

            response
        }
        Err(e) => {
            error!("Failed to open file: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn periodic_cleanup(state: SharedState) {
    let mut interval = interval(Duration::from_secs(CLEANUP_INTERVAL_SECONDS));

    loop {
        interval.tick().await;
        cleanup_old_data(&state).await;
    }
}

async fn cleanup_old_data(state: &SharedState) {
    let current_time = now_secs();

    // Cleanup old files
    match fs::read_dir(&*DOWNLOAD_DIR).await {
        Ok(mut entries) => {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_file() {
                    match entry.metadata().await {
                        Ok(metadata) => {
                            if let Ok(modified) = metadata.modified() {
                                let age_hours = 
                                    modified.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as f64
                                    / 3600.0;
                                let current_hours = current_time / 3600.0;
                                
                                if current_hours - age_hours > MAX_FILE_AGE_HOURS {
                                    if let Err(e) = fs::remove_file(&path).await {
                                        warn!("Failed to remove old file {:?}: {}", path, e);
                                    } else {
                                        info!("Cleaned up old file: {:?}", path);
                                    }
                                }
                            }
                        }
                        Err(e) => warn!("Failed to get metadata for {:?}: {}", path, e),
                    }
                }
            }
        }
        Err(e) => warn!("Failed to read download directory: {}", e),
    }

    // Cleanup old memory entries in single lock
    {
        let mut downloads = state.active_downloads.lock().await;
        let stale_ids: Vec<String> = downloads
            .iter()
            .filter(|(_, info)| {
                let age_hours = (current_time - info.timestamp) / 3600.0;
                age_hours > MAX_FILE_AGE_HOURS
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in stale_ids {
            downloads.remove(&id);
            info!("Cleaned up stale memory entry: {}", id);
        }
    }
}

fn sanitize_filename(name: &str) -> String {
    // Pre-allocate with input length to avoid reallocations
    let mut result = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_alphanumeric() || c == '.' || c == '_' || c == '-' || c == '(' || c == ')' {
            result.push(c);
        } else if c == ' ' {
            result.push('_');
        }
    }
    // Trim trailing whitespace from end
    while result.ends_with('_') {
        result.pop();
    }
    result
}

/// Check and update yt-dlp to the latest version
async fn update_yt_dlp() {
    info!("Checking for yt-dlp updates...");
    
    let output = Command::new("yt-dlp")
        .args(["-U"])
        .output()
        .await;
    
    match output {
        Ok(result) => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);
            
            if result.status.success() {
                if stdout.contains("up to date") {
                    info!("yt-dlp is already up to date");
                } else {
                    info!("yt-dlp updated successfully: {}", stdout.trim());
                }
            } else {
                warn!("yt-dlp update check failed: {} {}", stdout, stderr);
            }
        }
        Err(e) => {
            warn!("Failed to run yt-dlp -U: {}", e);
        }
    }
}

/// Periodically check for yt-dlp updates (every 24 hours)
async fn periodic_yt_dlp_update() {
    let mut interval = interval(Duration::from_secs(24 * 60 * 60)); // 24 hours
    
    loop {
        interval.tick().await;
        info!("Running periodic yt-dlp update check...");
        update_yt_dlp().await;
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ============================================================================
// Unit Tests
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename_basic() {
        assert_eq!(sanitize_filename("hello world"), "hello_world");
        assert_eq!(sanitize_filename("file.txt"), "file.txt");
        assert_eq!(sanitize_filename("my-video_123"), "my-video_123");
    }

    #[test]
    fn test_sanitize_filename_special_chars() {
        assert_eq!(sanitize_filename("hello/world"), "helloworld");
        assert_eq!(sanitize_filename("file\\name"), "filename");
        assert_eq!(sanitize_filename("video(1080p).mp4"), "video(1080p).mp4");
        assert_eq!(sanitize_filename("test[123]"), "test123");
    }

    #[test]
    fn test_sanitize_filename_trailing_underscores() {
        assert_eq!(sanitize_filename("hello world  "), "hello_world");
        assert_eq!(sanitize_filename("test_"), "test");
    }

    #[test]
    fn test_sanitize_filename_empty() {
        assert_eq!(sanitize_filename(""), "");
        assert_eq!(sanitize_filename("!!!"), "");
    }

    #[test]
    fn test_sanitize_filename_unicode() {
        // Unicode letters are kept (é, ö, and CJK are alphanumeric)
        assert_eq!(sanitize_filename("héllo wörld"), "héllo_wörld");
        assert_eq!(sanitize_filename("日本語"), "日本語");
        // Symbols and emoji are filtered out
        assert_eq!(sanitize_filename("hello🎉world"), "helloworld");
    }

    #[test]
    fn test_default_format() {
        assert_eq!(default_format(), "mp4");
    }

    #[test]
    fn test_default_quality() {
        assert_eq!(default_quality(), "best");
    }

    #[test]
    fn test_progress_regex_matches() {
        let test_cases = vec![
            ("[download]  50.5%", Some("50.5")),
            ("[download] 100%", Some("100")),
            ("[download]   0%", Some("0")),
            ("[download]  12.34% of 100MiB", Some("12.34")),
        ];

        for (input, expected) in test_cases {
            let caps = PROGRESS_REGEX.captures(input);
            if let Some(expected_val) = expected {
                assert!(caps.is_some(), "Should match: {}", input);
                assert_eq!(caps.unwrap().get(1).unwrap().as_str(), expected_val);
            }
        }
    }

    #[test]
    fn test_progress_regex_no_match() {
        let no_match_cases = vec![
            "[info] Downloading",
            "50% complete",
            "",
            "[download]",
        ];

        for input in no_match_cases {
            assert!(
                PROGRESS_REGEX.captures(input).is_none(),
                "Should not match: {}",
                input
            );
        }
    }

    #[test]
    fn test_download_dir_lazy() {
        // Test that DOWNLOAD_DIR is valid
        let dir = &*DOWNLOAD_DIR;
        assert!(dir.is_absolute() || dir.starts_with("."));
    }

    #[test]
    fn test_download_semaphore() {
        // Test semaphore initialization
        let permits = DOWNLOAD_SEMAPHORE.available_permits();
        // Default is 2, but could be overridden by env var
        assert!(permits > 0);
    }

    #[test]
    fn test_api_response_serialization() {
        let response: ApiResponse<String> = ApiResponse {
            success: true,
            data: Some("test data".to_string()),
            error: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"data\":\"test data\""));
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_api_response_error() {
        let response: ApiResponse<String> = ApiResponse {
            success: false,
            data: None,
            error: Some("error message".to_string()),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"error\":\"error message\""));
        assert!(!json.contains("data"));
    }

    #[test]
    fn test_format_info_serialization() {
        let info = FormatInfo {
            format: "mp4".to_string(),
            quality: "1080p".to_string(),
            label: "1080p (mp4)".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"format\":\"mp4\""));
        assert!(json.contains("\"quality\":\"1080p\""));
        assert!(json.contains("\"label\":\"1080p"));
    }

    #[test]
    fn test_progress_update_serialization() {
        let update = ProgressUpdate {
            status: "completed".to_string(),
            progress: 100.0,
            download_url: Some("/api/file/123".to_string()),
            error: None,
        };
        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("\"status\":\"completed\""));
        assert!(json.contains("\"progress\":100.0"));
        assert!(json.contains("\"download_url\":\"/api/file/123\""));
    }

    #[test]
    fn test_progress_update_skips_null() {
        let update = ProgressUpdate {
            status: "processing".to_string(),
            progress: 50.0,
            download_url: None,
            error: None,
        };
        let json = serde_json::to_string(&update).unwrap();
        assert!(!json.contains("download_url"));
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_download_request_deserialization() {
        let json = r#"{"url":"https://youtube.com/watch?v=test","format":"mp4","quality":"1080p"}"#;
        let request: DownloadRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.url, "https://youtube.com/watch?v=test");
        assert_eq!(request.format, "mp4");
        assert_eq!(request.quality, "1080p");
    }

    #[test]
    fn test_download_request_defaults() {
        let json = r#"{"url":"https://youtube.com/watch?v=test"}"#;
        let request: DownloadRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.url, "https://youtube.com/watch?v=test");
        assert_eq!(request.format, "mp4");
        assert_eq!(request.quality, "best");
    }

    #[tokio::test]
    async fn test_app_state_creation() {
        let state = Arc::new(AppState {
            active_downloads: Mutex::new(HashMap::new()),
        });

        // Test inserting a download
        {
            let mut downloads = state.active_downloads.lock().await;
            downloads.insert("test-id".to_string(), DownloadInfo {
                status: "queued".to_string(),
                progress: 0.0,
                file_path: None,
                file_name: None,
                error: None,
                timestamp: now_secs(),
            });
        }

        // Test reading it back
        {
            let downloads = state.active_downloads.lock().await;
            assert!(downloads.contains_key("test-id"));
            let info = downloads.get("test-id").unwrap();
            assert_eq!(info.status, "queued");
            assert_eq!(info.progress, 0.0);
        }
    }

    #[tokio::test]
    async fn test_download_info_update() {
        let state = Arc::new(AppState {
            active_downloads: Mutex::new(HashMap::new()),
        });

        // Insert
        {
            let mut downloads = state.active_downloads.lock().await;
            downloads.insert("test-id".to_string(), DownloadInfo {
                status: "queued".to_string(),
                progress: 0.0,
                file_path: None,
                file_name: None,
                error: None,
                timestamp: now_secs(),
            });
        }

        // Update progress
        {
            let mut downloads = state.active_downloads.lock().await;
            if let Some(info) = downloads.get_mut("test-id") {
                info.progress = 50.5;
                info.status = "processing".to_string();
            }
        }

        // Verify
        {
            let downloads = state.active_downloads.lock().await;
            let info = downloads.get("test-id").unwrap();
            assert_eq!(info.progress, 50.5);
            assert_eq!(info.status, "processing");
        }
    }

    #[test]
    fn test_constants() {
        assert_eq!(MAX_FILE_AGE_HOURS, 1.0);
        assert_eq!(CLEANUP_INTERVAL_SECONDS, 600);
    }

    #[test]
    fn test_temp_file_naming() {
        // Verify .part extension logic
        let file_name = "video.mp4";
        let temp_name = format!("{}.part", file_name);
        assert_eq!(temp_name, "video.mp4.part");

        let file_name = "song.mp3";
        let temp_name = format!("{}.part", file_name);
        assert_eq!(temp_name, "song.mp3.part");
    }

    #[test]
    fn test_unique_filename_generation() {
        // Test that we can generate unique filenames with timestamps
        let safe_title = "my_video";
        let ext = "mp4";
        let timestamp = 1234567890u64;
        let unique_name = format!("{}_{}.{}", safe_title, timestamp, ext);
        assert_eq!(unique_name, "my_video_1234567890.mp4");
    }
}
