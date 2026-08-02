use std::collections::HashMap;
use axum::{
    body::Body,
    extract::{ConnectInfo, Path, Query, State},
    http::StatusCode,
    response::{sse::Event, Html, IntoResponse, Json, Sse, Response},
};
use futures::stream::{Stream, StreamExt};
use regex::Regex;
use tokio::{
    fs,
    process::Command,
    time::{sleep, Duration},
};
use tracing::{error, info, warn};
use uuid::Uuid;
use sha2::{Digest, Sha256};

use crate::errors::{AppError, AppResult};
use crate::models::{
    ApiResponse, AnalyzeResponse, ConfigResponse, DownloadRequest, DownloadResponse,
    FormatInfo, LoginRequest, LoginResponse, PlaylistVideo, ProgressUpdate, ShareRequest,
};
use crate::state::{
    AppState, DownloadInfo, SharedState, PASSWORD_HASH, now_secs,
};
use crate::ytdlp::{
    get_audio_multiplier, map_audio_format_name,
    download_task, playlist_download_task,
};

// Static HTML frontend
static INDEX_HTML: &str = include_str!("../frontend.html");
static SW_JS: &str = include_str!("../sw.js");
static APP_ICON: &[u8] = include_bytes!("../app-icon.png");

pub async fn root() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub async fn manifest_handler() -> impl IntoResponse {
    let manifest = include_str!("../manifest.json");
    (
        [("Content-Type", "application/json")],
        manifest,
    )
}

pub async fn sw_handler() -> impl IntoResponse {
    (
        [("Content-Type", "application/javascript")],
        SW_JS,
    )
}

pub async fn icon_handler() -> impl IntoResponse {
    (
        [("Content-Type", "image/png")],
        APP_ICON,
    )
}

pub async fn handle_share(
    State(_state): State<SharedState>,
    Json(request): Json<ShareRequest>,
) -> Result<Html<String>, StatusCode> {
    let url_to_analyze = request.url.or_else(|| {
        request.text.and_then(|text| {
            let url_regex = Regex::new(r"https?://[^\s]+").unwrap();
            url_regex.find(&text).map(|m| m.as_str().to_string())
        })
    });

    let html_content = if let Some(url) = url_to_analyze {
        format!(
            r#"<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<script>
window.addEventListener('load', function() {{
    const input = document.querySelector('input[x-model="url"]');
    if (input) {{
        input.value = '{url}';
        setTimeout(() => {{
            window.athenaApp && window.athenaApp.analyze && window.athenaApp.analyze();
        }}, 100);
    }}
}});
</script>
</head>
<body>
<p>Processing shared URL...</p>
</body>
</html>"#,
            url = url.replace('\'', "\\'")
        )
    } else {
        INDEX_HTML.to_string()
    };

    Ok(Html(html_content))
}

pub async fn is_authenticated(
    headers: &axum::http::HeaderMap,
    query_token: Option<&str>,
    state: &AppState,
) -> bool {
    if PASSWORD_HASH.is_none() {
        return true;
    }

    let auth_header = headers.get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let token = auth_header.or(query_token);

    if let Some(token) = token {
        let tokens = state.auth_tokens.lock().await;
        tokens.contains_key(token)
    } else {
        false
    }
}

pub async fn get_config() -> Json<ApiResponse<ConfigResponse>> {
    Json(ApiResponse {
        success: true,
        data: Some(ConfigResponse {
            auth_enabled: PASSWORD_HASH.is_some(),
        }),
        error: None,
    })
}

pub async fn login(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(request): Json<LoginRequest>,
) -> AppResult<Json<ApiResponse<LoginResponse>>> {
    let client_ip = addr.ip().to_string();
    let now = now_secs();

    // 1. Password login rate limiting check (5 attempts in 15 mins)
    {
        let mut attempts = state.login_attempts.lock().await;
        if let Some(timestamps) = attempts.get_mut(&client_ip) {
            // Keep attempts within last 15 minutes (900 seconds)
            timestamps.retain(|&t| now - t < 900.0);
            if timestamps.len() >= 5 {
                warn!("Block login attempt from {} due to rate limiting", client_ip);
                return Err(AppError::Unauthorized {
                    message: "Zu viele Fehlversuche. Bitte 15 Minuten warten.".to_string(),
                });
            }
        }
    }

    if let Some(ref hash) = *PASSWORD_HASH {
        let mut hasher = Sha256::new();
        hasher.update(request.password.as_bytes());
        let input_hash = hex::encode(hasher.finalize());

        if input_hash == *hash {
            // Success: clear rate-limit attempts
            {
                let mut attempts = state.login_attempts.lock().await;
                attempts.remove(&client_ip);
            }

            // Generate token
            let token = Uuid::new_v4().to_string();
            {
                let mut tokens = state.auth_tokens.lock().await;
                tokens.insert(token.clone(), ());
            }
            info!("Login successful, token generated for {}", client_ip);
            return Ok(Json(ApiResponse {
                success: true,
                data: Some(LoginResponse { token }),
                error: None,
            }));
        } else {
            // Failure: record attempt
            {
                let mut attempts = state.login_attempts.lock().await;
                attempts.entry(client_ip.clone()).or_insert_with(Vec::new).push(now);
            }
            return Err(AppError::Unauthorized {
                message: "Invalid password".to_string(),
            });
        }
    }

    Ok(Json(ApiResponse {
        success: true,
        data: Some(LoginResponse { token: String::new() }),
        error: None,
    }))
}

pub async fn analyze_video(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<DownloadRequest>,
) -> AppResult<Json<ApiResponse<AnalyzeResponse>>> {
    if !is_authenticated(&headers, None, &state).await {
        return Err(AppError::Unauthorized {
            message: "Unauthorized".to_string(),
        });
    }

    info!("Analyzing video/playlist: {}", request.url);

    // 1. Perform a quick flat-playlist check to see if this is a playlist
    let flat_output = Command::new("yt-dlp")
        .args([
            "--quiet",
            "--no-warnings",
            "--flat-playlist",
            "--dump-single-json",
            &request.url,
        ])
        .output()
        .await;

    let mut is_playlist = false;
    let mut playlist_videos = None;
    let mut playlist_title = String::new();
    let mut playlist_author = String::new();
    let mut playlist_id = String::new();

    if let Ok(out) = flat_output {
        if out.status.success() {
            if let Ok(flat_json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if flat_json.get("_type").and_then(|t| t.as_str()) == Some("playlist") {
                    is_playlist = true;
                    playlist_title = flat_json.get("title").and_then(|t| t.as_str()).unwrap_or("Playlist").to_string();
                    playlist_author = flat_json.get("uploader").and_then(|t| t.as_str()).unwrap_or("Unknown").to_string();
                    playlist_id = flat_json.get("id").and_then(|t| t.as_str()).unwrap_or("playlist").to_string();

                    if let Some(entries) = flat_json.get("entries").and_then(|e| e.as_array()) {
                        let mut videos = Vec::new();
                        for entry in entries {
                            if let Some(id) = entry.get("id").and_then(|i| i.as_str()) {
                                let title = entry.get("title").and_then(|t| t.as_str()).unwrap_or("Unbekanntes Video").to_string();
                                let url = format!("https://www.youtube.com/watch?v={}", id);
                                videos.push(PlaylistVideo {
                                    id: id.to_string(),
                                    title,
                                    url,
                                });
                            }
                        }
                        playlist_videos = Some(videos);
                    }
                }
            }
        }
    }

    if is_playlist {
        let response = AnalyzeResponse {
            id: playlist_id,
            title: playlist_title,
            description: format!("Playlist mit {} Videos", playlist_videos.as_ref().map(|v| v.len()).unwrap_or(0)),
            thumbnail: "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%23434ed1'%3E%3Cpath d='M4 6H2v14c0 1.1.9 2 2 2h14v-2H4V6zm16-4H8c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm-8 12.5v-9l6 4.5-6 4.5z'/%3E%3C/svg%3E".to_string(),
            duration: String::new(),
            author: playlist_author,
            formats: vec![
                FormatInfo {
                    media_type: "video".to_string(),
                    format: "mp4".to_string(),
                    quality: "best".to_string(),
                    label: "Beste Qualität (Video)".to_string(),
                    filesize: None,
                },
                FormatInfo {
                    media_type: "audio".to_string(),
                    format: "best".to_string(),
                    quality: "best".to_string(),
                    label: "Beste Qualität (Audio)".to_string(),
                    filesize: None,
                }
            ],
            playlist_videos,
        };
        return Ok(Json(ApiResponse {
            success: true,
            data: Some(response),
            error: None,
        }));
    }

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
        .map_err(|e| AppError::ExternalCommand {
            message: format!("Failed to execute yt-dlp: {}", e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("yt-dlp analyze failed: {}", stderr);
        return Err(AppError::ExternalCommand {
            message: "Failed to analyze video".to_string(),
        });
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let info: serde_json::Value = serde_json::from_str(&json_str)?;

    let duration_secs = info.get("duration")
        .and_then(|d| d.as_f64())
        .unwrap_or(0.0) as u64;
    let duration_mins = duration_secs / 60;
    let duration_rem = duration_secs % 60;
    let duration = format!("{}:{:02}", duration_mins, duration_rem);

    let mut parsed_video_formats = Vec::new();
    let mut seen_video = std::collections::HashSet::new();

    let mut parsed_audio_formats = Vec::new();
    let mut seen_audio = std::collections::HashSet::new();

    if let Some(formats) = info.get("formats").and_then(|f| f.as_array()) {
        for fmt in formats {
            let vcodec = fmt.get("vcodec").and_then(|v| v.as_str()).unwrap_or("none");
            let acodec = fmt.get("acodec").and_then(|a| a.as_str()).unwrap_or("none");
            let ext = fmt.get("ext").and_then(|e| e.as_str()).unwrap_or("unknown");

            if ext == "mhtml" || ext == "unknown" {
                continue;
            }

            if vcodec != "none" {
                let height = fmt.get("height").and_then(|h| h.as_u64()).unwrap_or(0);
                if height > 0 {
                    let key = (height, ext.to_string());
                    if seen_video.insert(key) {
                        parsed_video_formats.push((height, ext.to_string()));
                    }
                }
            } else if acodec != "none" {
                let format_id = fmt.get("format_id").and_then(|id| id.as_str()).unwrap_or("unknown");
                if format_id != "unknown" {
                    let abr = fmt.get("abr").and_then(|a| a.as_f64())
                        .or_else(|| fmt.get("tbr").and_then(|t| t.as_f64()))
                        .unwrap_or(0.0);

                    let mapped_format = map_audio_format_name(acodec, ext);
                    let multiplier = get_audio_multiplier(acodec, ext);
                    let perceived_score = abr * multiplier;

                    let key = (mapped_format.clone(), format_id.to_string());
                    if seen_audio.insert(key) {
                        parsed_audio_formats.push((format_id.to_string(), mapped_format, abr, perceived_score));
                    }
                }
            }
        }
    }

    parsed_video_formats.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1))
    });

    parsed_audio_formats.sort_by(|a, b| {
        b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut all_formats = Vec::new();

    fn format_filesize(bytes: f64) -> Option<String> {
        if bytes <= 0.0 { return None; }
        if bytes < 1024.0 * 1024.0 {
            Some(format!("{:.1} KB", bytes / 1024.0))
        } else if bytes < 1024.0 * 1024.0 * 1024.0 {
            Some(format!("{:.1} MB", bytes / (1024.0 * 1024.0)))
        } else {
            Some(format!("{:.2} GB", bytes / (1024.0 * 1024.0 * 1024.0)))
        }
    }

    all_formats.push(FormatInfo {
        media_type: "video".to_string(),
        format: "mp4/webm".to_string(),
        quality: "best".to_string(),
        label: "Beste Qualität (Video)".to_string(),
        filesize: None,
    });

    for (height, ext) in parsed_video_formats {
        let filesize = info.get("formats")
            .and_then(|f| f.as_array())
            .and_then(|arr| arr.iter().find(|fmt| {
                fmt.get("height").and_then(|h| h.as_u64()) == Some(height) &&
                fmt.get("ext").and_then(|e| e.as_str()) == Some(&ext)
            }))
            .and_then(|fmt| {
                fmt.get("filesize").and_then(|fs| fs.as_f64())
                    .or_else(|| fmt.get("filesize_approx").and_then(|fs| fs.as_f64()))
            })
            .and_then(|fs| format_filesize(fs));

        all_formats.push(FormatInfo {
            media_type: "video".to_string(),
            format: ext.clone(),
            quality: format!("{}p-{}", height, ext),
            label: format!("{}p ({})", height, ext),
            filesize,
        });
    }

    all_formats.push(FormatInfo {
        media_type: "audio".to_string(),
        format: "best".to_string(),
        quality: "best".to_string(),
        label: "Beste Qualität (Audio)".to_string(),
        filesize: None,
    });

    for (format_id, format_name, abr, _) in parsed_audio_formats {
        let label = if abr > 0.0 {
            format!("{} (~{:.0} kbps)", format_name.to_uppercase(), abr)
        } else {
            format_name.to_uppercase()
        };

        let filesize = info.get("formats")
            .and_then(|f| f.as_array())
            .and_then(|arr| arr.iter().find(|fmt| {
                fmt.get("format_id").and_then(|id| id.as_str()) == Some(&format_id)
            }))
            .and_then(|fmt| {
                fmt.get("filesize").and_then(|fs| fs.as_f64())
                    .or_else(|| fmt.get("filesize_approx").and_then(|fs| fs.as_f64()))
            })
            .and_then(|fs| format_filesize(fs));

        all_formats.push(FormatInfo {
            media_type: "audio".to_string(),
            format: format_name,
            quality: format_id,
            label,
            filesize,
        });
    }

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
        playlist_videos: None,
    };

    Ok(Json(ApiResponse {
        success: true,
        data: Some(response),
        error: None,
    }))
}

pub async fn trigger_ytdlp_update(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
) -> AppResult<Json<ApiResponse<serde_json::Value>>> {
    if !is_authenticated(&headers, None, &state).await {
        return Err(AppError::Unauthorized {
            message: "Unauthorized".to_string(),
        });
    }

    let update_result = Command::new("yt-dlp")
        .args(["-U"])
        .output()
        .await;

    match update_result {
        Ok(result) if result.status.success() => {
            let stdout = String::from_utf8_lossy(&result.stdout);

            if stdout.contains("up to date") || stdout.is_empty() {
                info!("yt-dlp update check: already current");
                Ok(Json(ApiResponse {
                    success: true,
                    data: Some(serde_json::json!({ "status": "current" })),
                    error: None,
                }))
            } else {
                info!("yt-dlp update check: update performed");
                Ok(Json(ApiResponse {
                    success: true,
                    data: Some(serde_json::json!({ "status": "updated" })),
                    error: None,
                }))
            }
        }
        Ok(_) => {
            warn!("yt-dlp update check failed");
            Err(AppError::ExternalCommand {
                message: "Update check failed".to_string(),
            })
        }
        Err(e) => {
            warn!("Failed to run yt-dlp update: {}", e);
            Err(AppError::ExternalCommand {
                message: format!("Update check failed: {}", e),
            })
        }
    }
}

pub async fn start_download(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<DownloadRequest>,
) -> AppResult<Json<ApiResponse<DownloadResponse>>> {
    if !is_authenticated(&headers, None, &state).await {
        return Err(AppError::Unauthorized {
            message: "Unauthorized".to_string(),
        });
    }

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
            speed: None,
            eta: None,
        });
    }

    let state_clone = state.clone();
    let download_id_clone = download_id.clone();
    tokio::spawn(async move {
        if let Some(urls) = request.playlist_urls {
            if !urls.is_empty() {
                playlist_download_task(
                    state_clone,
                    download_id_clone,
                    urls,
                    request.format,
                    request.quality,
                )
                .await;
                return;
            }
        }

        download_task(
            state_clone,
            download_id_clone,
            request.url,
            request.format,
            request.quality,
        )
        .await;
    });

    Ok(Json(ApiResponse {
        success: true,
        data: Some(DownloadResponse {
            download_id,
            status: "queued".to_string(),
        }),
        error: None,
    }))
}

pub async fn progress_stream(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(download_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let query_token = params.get("token").map(|s| s.as_str());
    let authenticated = is_authenticated(&headers, query_token, &state).await;

    let stream = async_stream::stream! {
        if !authenticated {
            let error_json = serde_json::to_string(&ProgressUpdate {
                status: "error".to_string(),
                progress: 0.0,
                download_url: None,
                error: Some("Unauthorized".to_string()),
                speed: None,
                eta: None,
            }).unwrap_or_default();
            yield Ok(Event::default().data(error_json));
            return;
        }

        loop {
            let (status, progress, file_path, error, speed, eta) = {
                let downloads = state.active_downloads.lock().await;
                match downloads.get(&download_id) {
                    Some(info) => (
                        info.status.clone(),
                        info.progress,
                        info.file_path.clone(),
                        info.error.clone(),
                        info.speed.clone(),
                        info.eta.clone(),
                    ),
                    None => {
                        let json = serde_json::to_string(&ProgressUpdate {
                            status: String::from("error"),
                            progress: 0.0,
                            download_url: None,
                            error: Some(String::from("Download not found")),
                            speed: None,
                            eta: None,
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
                speed,
                eta,
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

pub async fn download_file(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(download_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Response> {
    let query_token = params.get("token").map(|s| s.as_str());

    if !is_authenticated(&headers, query_token, &state).await {
        return Err(AppError::Unauthorized {
            message: "Unauthorized".to_string(),
        });
    }

    let download_info = {
        let downloads = state.active_downloads.lock().await;
        downloads.get(&download_id).cloned()
    };

    let info = match download_info {
        Some(i) if i.status == "completed" => i,
        Some(i) => {
            return Err(AppError::DownloadNotReady { status: i.status });
        }
        None => {
            return Err(AppError::DownloadNotFound { id: download_id });
        }
    };

    let file_path = match info.file_path {
        Some(p) => p,
        None => {
            return Err(AppError::FileNotFound { path: "Path not set in info".to_string() });
        }
    };

    if !file_path.exists() {
        return Err(AppError::FileNotFound { path: file_path.to_string_lossy().to_string() });
    }

    let file_name = info.file_name
        .map(|name| {
            let id_marker = format!("-[{}].", download_id);
            name.replace(&id_marker, ".")
        })
        .unwrap_or_else(|| download_id.clone());
    let file_size = fs::metadata(&file_path).await?.len();

    let file_path_clone = file_path.clone();
    let download_id_clone = download_id.clone();

    let file = tokio::fs::File::open(&file_path).await?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let wrapped_stream = stream.then(move |chunk| {
        let path = file_path_clone.clone();
        let _id = download_id_clone.clone();
        async move {
            if chunk.is_err() {
                let _ = fs::remove_file(&path).await;
            }
            chunk
        }
    });
    let body = Body::from_stream(wrapped_stream);

    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert("content-type", "application/octet-stream".parse().unwrap());
    headers.insert("content-disposition", format!("attachment; filename=\"{}\"", file_name).parse().unwrap());
    headers.insert("content-length", file_size.to_string().parse().unwrap());

    tokio::spawn(async move {
        sleep(Duration::from_secs(2)).await;
        if let Err(e) = fs::remove_file(&file_path).await {
            warn!("Failed to remove temp file {:?}: {}", file_path, e);
        } else {
            info!("Removed temp file for download {}: {:?}", download_id, file_path);
        }
        let mut downloads = state.active_downloads.lock().await;
        downloads.remove(&download_id);
    });

    Ok(response)
}
