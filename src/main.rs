use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{sse::Event, Html, Json, Sse, Response, IntoResponse},
    routing::{get, post},
    Router,
};
use futures::stream::Stream;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs,
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{Mutex, Semaphore},
    time::{interval, sleep},
};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use uuid::Uuid;

// Static HTML frontend (embedded from Python version)
static INDEX_HTML: &str = include_str!("../frontend.html");

// Configuration
static DOWNLOAD_DIR: Lazy<PathBuf> = Lazy::new(|| {
    std::env::var("DOWNLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./downloads"))
});

const MAX_FILE_AGE_HOURS: f64 = 24.0;
const CLEANUP_INTERVAL_SECONDS: u64 = 3600;

// Global semaphore for concurrent downloads
static DOWNLOAD_SEMAPHORE: Lazy<Arc<Semaphore>> = Lazy::new(|| {
    let max = std::env::var("MAX_CONCURRENT_DOWNLOADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    Arc::new(Semaphore::new(max))
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

fn default_format() -> String { "mp4".to_string() }
fn default_quality() -> String { "best".to_string() }

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

    // Initialize state
    let state = Arc::new(AppState {
        active_downloads: Mutex::new(HashMap::new()),
    });

    // Start cleanup task
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        periodic_cleanup(cleanup_state).await;
    });

    // Build router
    let app = Router::new()
        .route("/", get(root))
        .route("/api/analyze", post(analyze_video))
        .route("/api/download", post(start_download))
        .route("/api/progress/:download_id", get(progress_stream))
        .route("/api/file/:download_id", get(download_file))
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

    // Extract formats
    let mut video_formats = Vec::new();
    let mut audio_formats = Vec::new();
    let mut seen_qualities = std::collections::HashSet::new();

    if let Some(formats) = info.get("formats").and_then(|f| f.as_array()) {
        for fmt in formats {
            let vcodec = fmt.get("vcodec").and_then(|v| v.as_str()).unwrap_or("none");
            let acodec = fmt.get("acodec").and_then(|a| a.as_str()).unwrap_or("none");
            let resolution = fmt.get("resolution").and_then(|r| r.as_str()).unwrap_or("unknown");
            let ext = fmt.get("ext").and_then(|e| e.as_str()).unwrap_or("unknown");
            let _format_note = fmt.get("format_note").and_then(|f| f.as_str()).unwrap_or(resolution);

            if vcodec != "none" && resolution != "unknown" {
                let key = format!("{}-{}", resolution, ext);
                if seen_qualities.insert(key.clone()) {
                    video_formats.push(FormatInfo {
                        format: ext.to_string(),
                        quality: resolution.to_string(),
                        label: format!("{} ({})", resolution, ext),
                    });
                }
            }

            if acodec != "none" && vcodec == "none" {
                if !audio_formats.iter().any(|f: &FormatInfo| f.format == "mp3") {
                    audio_formats.push(FormatInfo {
                        format: "mp3".to_string(),
                        quality: "best".to_string(),
                        label: "MP3 Audio (Best)".to_string(),
                    });
                }
            }
        }
    }

    // Limit formats
    video_formats.truncate(4);
    audio_formats.truncate(1);
    let mut all_formats = video_formats;
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
    let output_template = DOWNLOAD_DIR.join("%(title)s.%(ext)s");
    let template_str = output_template.to_str().ok_or("Invalid path")?;

    // Build format string
    let format_arg = if format_type == "mp3" {
        "bestaudio/best".to_string()
    } else if quality == "best" {
        format!("best[ext={}]/best", format_type)
    } else {
        let height: String = quality.chars().filter(|c| c.is_ascii_digit()).collect();
        format!("best[height<={}][ext={}]/best[height<={}]/best", height, format_type, height)
    };

    let mut args = vec![
        "--quiet",
        "--no-warnings",
        "--newline",
        "--progress",
        "-f", &format_arg,
        "-o", template_str,
    ];

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

    // Progress regex patterns
    let progress_regex = Regex::new(r"\[download\]\s+(\d+\.?\d*)%").unwrap();

    // Read progress lines
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(cap) = progress_regex.captures(&line) {
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

    let status = child.wait().await.map_err(|e| format!("Failed to wait for process: {}", e))?;

    if !status.success() {
        return Err("yt-dlp process failed".to_string());
    }

    // Get video info to determine filename
    let info_output = Command::new("yt-dlp")
        .args(["--quiet", "--no-warnings", "--dump-json", "--no-download", url])
        .output()
        .await
        .map_err(|e| format!("Failed to get video info: {}", e))?;

    let title = if info_output.status.success() {
        let json_str = String::from_utf8_lossy(&info_output.stdout);
        serde_json::from_str::<serde_json::Value>(&json_str)
            .ok()
            .and_then(|v| v.get("title").and_then(|t| t.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "download".to_string())
    } else {
        "download".to_string()
    };

    let ext = if format_type == "mp3" { "mp3" } else { format_type };
    let file_name = sanitize_filename(&format!("{}.{}"
, title, ext));

    // Find the actual downloaded file
    let _base_path = DOWNLOAD_DIR.join(&title);
    let _pattern = format!("{}*", title);
    let mut entries = fs::read_dir(&*DOWNLOAD_DIR)
        .await
        .map_err(|e| format!("Failed to read download dir: {}", e))?;

    let mut actual_file: Option<PathBuf> = None;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if let Some(name) = path.file_stem() {
            if name.to_string_lossy().starts_with(&title) {
                actual_file = Some(path);
                break;
            }
        }
    }

    {
        let mut downloads = state.active_downloads.lock().await;
        if let Some(info) = downloads.get_mut(download_id) {
            info.status = "completed".to_string();
            info.progress = 100.0;
            info.file_path = actual_file.clone();
            info.file_name = Some(file_name);
        }
    }

    info!("Download {} completed: {:?}", download_id, actual_file);
    Ok(())
}

async fn progress_stream(
    State(state): State<SharedState>,
    Path(download_id): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let state = state.clone();
    let id = download_id.clone();

    // Create the SSE stream
    let stream = async_stream::stream! {
        loop {
            let update = {
                let downloads = state.active_downloads.lock().await;
                match downloads.get(&id) {
                    Some(info) => {
                        let download_url = if info.status == "completed" && info.file_path.is_some() {
                            Some(format!("/api/file/{}", id))
                        } else {
                            None
                        };
                        
                        ProgressUpdate {
                            status: info.status.clone(),
                            progress: info.progress,
                            download_url,
                            error: info.error.clone(),
                        }
                    }
                    None => {
                        ProgressUpdate {
                            status: "error".to_string(),
                            progress: 0.0,
                            download_url: None,
                            error: Some("Download not found".to_string()),
                        }
                    }
                }
            };

            let should_break = update.status == "completed" || update.status == "error";
            
            let json = serde_json::to_string(&update).unwrap_or_default();
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

    match fs::read(&file_path).await {
        Ok(contents) => {
            let file_name = info.file_name.unwrap_or_else(|| download_id.clone());
            let headers = [
                ("content-type", "application/octet-stream"),
                ("content-disposition", &format!("attachment; filename=\"{}\"", file_name)),
            ];
            
            (headers, contents).into_response()
        }
        Err(e) => {
            error!("Failed to read file: {}", e);
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

    // Cleanup old memory entries
    let stale_ids: Vec<String> = {
        let downloads = state.active_downloads.lock().await;
        downloads
            .iter()
            .filter(|(_, info)| {
                let age_hours = (current_time - info.timestamp) / 3600.0;
                age_hours > MAX_FILE_AGE_HOURS
            })
            .map(|(id, _)| id.clone())
            .collect()
    };

    if !stale_ids.is_empty() {
        let mut downloads = state.active_downloads.lock().await;
        for id in stale_ids {
            downloads.remove(&id);
            info!("Cleaned up stale memory entry: {}", id);
        }
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '.' || *c == '_' || *c == '-')
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
