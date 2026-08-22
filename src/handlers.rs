use axum::{
    body::Body,
    extract::{ConnectInfo, Path, Query, State},
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        StatusCode,
    },
    response::{sse::Event, Html, IntoResponse, Json, Redirect, Response, Sse},
};
use futures::stream::{Stream, StreamExt};
use once_cell::sync::Lazy;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::{
    fs,
    process::Command,
    time::{sleep, Duration},
};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::{
    AnalyzeResponse, ApiResponse, ConfigResponse, DownloadRequest, DownloadResponse, FormatInfo,
    LoginRequest, LoginResponse, PlaylistVideo, ProgressUpdate, ShareRequest,
};
use crate::state::{
    now_secs, AppState, DownloadInfo, SharedState, PASSWORD_HASH, TOKEN_MAX_AGE_SECS,
};
use crate::ytdlp::{
    download_task, get_audio_multiplier, map_audio_format_name, playlist_download_task,
};

static INDEX_HTML: &str = include_str!("../frontend.html");
static SW_JS: &str = include_str!("../sw.js");
static APP_ICON: &[u8] = include_bytes!("../app-icon.png");

static SHARE_URL_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r#"https?://[^\s"'<>]+"#).unwrap());

const CACHE_MANIFEST: &str = "public, max-age=300";
const CACHE_SW: &str = "no-cache";
const CACHE_ICON: &str = "public, max-age=604800";

pub async fn root() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub async fn manifest_handler() -> impl IntoResponse {
    let manifest = include_str!("../manifest.json");
    (
        [
            (CONTENT_TYPE, "application/json"),
            (CACHE_CONTROL, CACHE_MANIFEST),
        ],
        manifest,
    )
}

pub async fn sw_handler() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "application/javascript"),
            (CACHE_CONTROL, CACHE_SW),
        ],
        SW_JS,
    )
}

pub async fn icon_handler() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "image/png"), (CACHE_CONTROL, CACHE_ICON)],
        APP_ICON,
    )
}

fn validate_url(url: &str) -> AppResult<()> {
    if (url.starts_with("http://") || url.starts_with("https://")) && url.len() <= 2048 {
        Ok(())
    } else {
        Err(AppError::InvalidUrl {
            message: "Nur vollständige http(s)-URLs werden unterstützt".to_string(),
        })
    }
}

fn percent_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

fn content_disposition_value(file_name: &str) -> String {
    let ascii_fallback: String = file_name
        .chars()
        .take(120)
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii_fallback = ascii_fallback.trim_matches(|c: char| c == '_' || c == '.' || c == ' ');
    let fallback = if ascii_fallback.is_empty() {
        "download"
    } else {
        ascii_fallback
    };
    format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        fallback,
        percent_encode(file_name)
    )
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn is_permission_problem(detail: &str) -> bool {
    let d = detail.to_lowercase();
    d.contains("unable to write")
        || d.contains("permission")
        || d.contains("administrator")
        || d.contains("access denied")
        || d.contains("eacces")
}

fn extract_command_detail(stdout: &str, stderr: &str) -> String {
    let source = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let line = source
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.is_empty() {
        return "unbekannter Fehler".to_string();
    }
    let truncated: String = line.chars().take(200).collect();
    truncated.to_string()
}

pub async fn handle_share(Json(request): Json<ShareRequest>) -> Redirect {
    let candidate = request.url.or_else(|| {
        request
            .text
            .as_deref()
            .and_then(|text| SHARE_URL_REGEX.find(text).map(|m| m.as_str().to_string()))
    });

    match candidate.filter(|u| u.starts_with("http://") || u.starts_with("https://")) {
        Some(url) => Redirect::to(&format!("/?share={}", percent_encode(&url))),
        None => Redirect::to("/"),
    }
}

pub async fn is_authenticated(
    headers: &axum::http::HeaderMap,
    query_token: Option<&str>,
    state: &AppState,
) -> bool {
    if PASSWORD_HASH.is_none() {
        return true;
    }

    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let token = auth_header.or(query_token);

    if let Some(token) = token {
        let tokens = state.auth_tokens.lock().await;
        if let Some(created) = tokens.get(token) {
            return now_secs() - created < TOKEN_MAX_AGE_SECS;
        }
    }
    false
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

    {
        let mut attempts = state.login_attempts.lock().await;
        if let Some(timestamps) = attempts.get_mut(&client_ip) {
            timestamps.retain(|&t| now - t < 900.0);
            if timestamps.len() >= 5 {
                warn!(
                    "Block login attempt from {} due to rate limiting",
                    client_ip
                );
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

        if constant_time_eq(&input_hash, hash) {
            {
                let mut attempts = state.login_attempts.lock().await;
                attempts.remove(&client_ip);
            }

            let token = Uuid::new_v4().to_string();
            {
                let mut tokens = state.auth_tokens.lock().await;
                tokens.insert(token.clone(), now);
            }
            info!("Login successful, token generated for {}", client_ip);
            return Ok(Json(ApiResponse {
                success: true,
                data: Some(LoginResponse { token }),
                error: None,
            }));
        }

        {
            let mut attempts = state.login_attempts.lock().await;
            attempts.entry(client_ip.clone()).or_default().push(now);
        }
        return Err(AppError::Unauthorized {
            message: "Invalid password".to_string(),
        });
    }

    Ok(Json(ApiResponse {
        success: true,
        data: Some(LoginResponse {
            token: String::new(),
        }),
        error: None,
    }))
}

fn format_filesize(bytes: f64) -> Option<String> {
    if bytes <= 0.0 {
        return None;
    }
    if bytes < 1024.0 * 1024.0 {
        Some(format!("{:.1} KB", bytes / 1024.0))
    } else if bytes < 1024.0 * 1024.0 * 1024.0 {
        Some(format!("{:.1} MB", bytes / (1024.0 * 1024.0)))
    } else {
        Some(format!("{:.2} GB", bytes / (1024.0 * 1024.0 * 1024.0)))
    }
}

/// Build the user-facing format preset list from a raw yt-dlp info JSON object.
///
/// yt-dlp exposes every raw stream, including DASH/HLS duplicates of the same
/// quality under different format_ids. Streams are grouped into distinct
/// presets here so the UI never sees raw stream arrays:
/// - video: one preset per (height, container), sorted by height descending
/// - audio: one preset per (codec, ~10 kbps bucket), best variant per group,
///   sorted by perceived quality descending
fn build_format_presets(info: &serde_json::Value) -> Vec<FormatInfo> {
    let mut parsed_video_formats = Vec::new();
    let mut seen_video = std::collections::HashSet::new();

    // Audio presets grouped by (codec, ~10kbps bucket): YouTube exposes the same
    // stream multiple times (DASH/HLS duplicates with distinct format_ids), so
    // grouping must not rely on format_id. Keep the best variant per preset.
    let mut audio_presets: HashMap<(String, u64), (String, String, f64, f64)> = HashMap::new();

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
                let format_id = fmt
                    .get("format_id")
                    .and_then(|id| id.as_str())
                    .unwrap_or("unknown");
                if format_id != "unknown" {
                    let abr = fmt
                        .get("abr")
                        .and_then(|a| a.as_f64())
                        .or_else(|| fmt.get("tbr").and_then(|t| t.as_f64()))
                        .unwrap_or(0.0);

                    let mapped_format = map_audio_format_name(acodec, ext);
                    let multiplier = get_audio_multiplier(acodec, ext);
                    let perceived_score = abr * multiplier;

                    let abr_bucket = (abr / 10.0).round().max(1.0) as u64;
                    let key = (mapped_format.clone(), abr_bucket);
                    match audio_presets.get_mut(&key) {
                        Some(entry) => {
                            if perceived_score > entry.3 {
                                *entry = (
                                    format_id.to_string(),
                                    mapped_format,
                                    abr,
                                    perceived_score,
                                );
                            }
                        }
                        None => {
                            audio_presets.insert(
                                key,
                                (
                                    format_id.to_string(),
                                    mapped_format,
                                    abr,
                                    perceived_score,
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    parsed_video_formats.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let mut parsed_audio_formats: Vec<_> = audio_presets.into_values().collect();
    parsed_audio_formats.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));

    let mut all_formats = Vec::new();

    all_formats.push(FormatInfo {
        media_type: "video".to_string(),
        format: "mp4/webm".to_string(),
        quality: "best".to_string(),
        label: "Beste Qualität (Video)".to_string(),
        filesize: None,
    });

    for (height, ext) in parsed_video_formats {
        let filesize = info
            .get("formats")
            .and_then(|f| f.as_array())
            .and_then(|arr| {
                arr.iter().find(|fmt| {
                    fmt.get("height").and_then(|h| h.as_u64()) == Some(height)
                        && fmt.get("ext").and_then(|e| e.as_str()) == Some(&ext)
                })
            })
            .and_then(|fmt| {
                fmt.get("filesize")
                    .and_then(|fs| fs.as_f64())
                    .or_else(|| fmt.get("filesize_approx").and_then(|fs| fs.as_f64()))
            })
            .and_then(format_filesize);

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

        let filesize = info
            .get("formats")
            .and_then(|f| f.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|fmt| fmt.get("format_id").and_then(|id| id.as_str()) == Some(&format_id))
            })
            .and_then(|fmt| {
                fmt.get("filesize")
                    .and_then(|fs| fs.as_f64())
                    .or_else(|| fmt.get("filesize_approx").and_then(|fs| fs.as_f64()))
            })
            .and_then(format_filesize);

        all_formats.push(FormatInfo {
            media_type: "audio".to_string(),
            format: format_name,
            quality: format_id,
            label,
            filesize,
        });
    }

    all_formats
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

    validate_url(&request.url)?;
    info!("Analyzing video/playlist: {}", request.url);

    let flat_output = tokio::time::timeout(
        Duration::from_secs(45),
        Command::new("yt-dlp")
            .args([
                "--quiet",
                "--no-warnings",
                "--flat-playlist",
                "--dump-single-json",
                &request.url,
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()
    .and_then(|res| res.ok());

    let mut is_playlist = false;
    let mut playlist_videos = None;
    let mut playlist_title = String::new();
    let mut playlist_author = String::new();
    let mut playlist_id = String::new();

    if let Some(out) = flat_output {
        if out.status.success() {
            if let Ok(flat_json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if flat_json.get("_type").and_then(|t| t.as_str()) == Some("playlist") {
                    is_playlist = true;
                    playlist_title = flat_json
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Playlist")
                        .to_string();
                    playlist_author = flat_json
                        .get("uploader")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    playlist_id = flat_json
                        .get("id")
                        .and_then(|t| t.as_str())
                        .unwrap_or("playlist")
                        .to_string();

                    if let Some(entries) = flat_json.get("entries").and_then(|e| e.as_array()) {
                        let mut videos = Vec::new();
                        for entry in entries {
                            if let Some(id) = entry.get("id").and_then(|i| i.as_str()) {
                                let title = entry
                                    .get("title")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("Unbekanntes Video")
                                    .to_string();
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
            description: format!(
                "Playlist mit {} Videos",
                playlist_videos.as_ref().map(|v| v.len()).unwrap_or(0)
            ),
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
                },
            ],
            playlist_videos,
        };
        return Ok(Json(ApiResponse {
            success: true,
            data: Some(response),
            error: None,
        }));
    }

    let output = tokio::time::timeout(
        Duration::from_secs(90),
        Command::new("yt-dlp")
            .args([
                "--quiet",
                "--no-warnings",
                "--dump-json",
                "--no-download",
                &request.url,
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await;

    let output = match output {
        Ok(res) => res.map_err(|e| AppError::ExternalCommand {
            message: format!("Failed to execute yt-dlp: {}", e),
        })?,
        Err(_) => {
            error!("yt-dlp analyze timed out for {}", request.url);
            return Err(AppError::ExternalCommand {
                message: "Zeitüberschreitung bei der Video-Analyse".to_string(),
            });
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("yt-dlp analyze failed: {}", stderr);
        return Err(AppError::ExternalCommand {
            message: "Failed to analyze video".to_string(),
        });
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let info: serde_json::Value = serde_json::from_str(&json_str)?;

    let duration_secs = info.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0) as u64;
    let duration_mins = duration_secs / 60;
    let duration_rem = duration_secs % 60;
    let duration = format!("{}:{:02}", duration_mins, duration_rem);

    let all_formats = build_format_presets(&info);

    let description = info
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect::<String>();

    let response = AnalyzeResponse {
        id: info
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("unknown")
            .to_string(),
        title: info
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("Unknown")
            .to_string(),
        description,
        thumbnail: info
            .get("thumbnail")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        duration,
        author: info
            .get("uploader")
            .and_then(|u| u.as_str())
            .unwrap_or("Unknown")
            .to_string(),
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

    let update_result = tokio::time::timeout(
        Duration::from_secs(180),
        Command::new("yt-dlp")
            .args(["-U"])
            .kill_on_drop(true)
            .output(),
    )
    .await;

    match update_result {
        Ok(Ok(result)) if result.status.success() => {
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
        Ok(Ok(result)) => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);
            let detail = extract_command_detail(&stdout, &stderr);
            warn!("yt-dlp update failed: {}", detail);
            let message = if is_permission_problem(&detail) {
                format!(
                    "{} – der Server-User darf die yt-dlp-Binary nicht ersetzen. \
                     Bitte auf dem Server ausführen: sudo yt-dlp -U",
                    detail
                )
            } else {
                format!("yt-dlp Update fehlgeschlagen: {}", detail)
            };
            Err(AppError::ExternalCommand { message })
        }
        Ok(Err(e)) => {
            warn!("Failed to run yt-dlp update: {}", e);
            Err(AppError::ExternalCommand {
                message: format!("yt-dlp konnte nicht ausgeführt werden: {}", e),
            })
        }
        Err(_) => {
            warn!("yt-dlp update timed out");
            Err(AppError::ExternalCommand {
                message: "Zeitüberschreitung beim Update (180s)".to_string(),
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

    validate_url(&request.url)?;
    if let Some(urls) = &request.playlist_urls {
        for url in urls {
            validate_url(url)?;
        }
    }

    let download_id = Uuid::new_v4().to_string()[..8].to_string();
    info!("Starting download {} for URL: {}", download_id, request.url);

    {
        let mut downloads = state.active_downloads.lock().await;
        downloads.insert(
            download_id.clone(),
            DownloadInfo {
                status: "queued".to_string(),
                progress: 0.0,
                file_path: None,
                file_name: None,
                error: None,
                timestamp: now_secs(),
                speed: None,
                eta: None,
            },
        );
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
            return Err(AppError::FileNotFound {
                path: "Path not set in info".to_string(),
            });
        }
    };

    if !file_path.exists() {
        return Err(AppError::FileNotFound {
            path: file_path.to_string_lossy().to_string(),
        });
    }

    let raw_name = info.file_name.unwrap_or_else(|| download_id.clone());
    let id_marker = format!("-[{}].", download_id);
    let clean_name = raw_name.replace(&id_marker, ".");

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

    let disposition = content_disposition_value(&clean_name);

    let mut response = (StatusCode::OK, body).into_response();
    let response_headers = response.headers_mut();
    response_headers.insert(CONTENT_TYPE, "application/octet-stream".parse().unwrap());
    response_headers.insert(
        "content-disposition",
        disposition.parse().map_err(|_| AppError::Internal {
            message: "Invalid content disposition".to_string(),
        })?,
    );
    response_headers.insert("content-length", file_size.to_string().parse().unwrap());
    response_headers.insert("x-content-type-options", "nosniff".parse().unwrap());

    tokio::spawn(async move {
        sleep(Duration::from_secs(5)).await;
        if let Err(e) = fs::remove_file(&file_path).await {
            warn!("Failed to remove temp file {:?}: {}", file_path, e);
        } else {
            info!(
                "Removed temp file for download {}: {:?}",
                download_id, file_path
            );
        }
        let mut downloads = state.active_downloads.lock().await;
        downloads.remove(&download_id);
    });

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- validate_url ----------

    #[test]
    fn test_validate_url_accepts_http() {
        assert!(validate_url("http://example.com").is_ok());
    }

    #[test]
    fn test_validate_url_accepts_https() {
        assert!(validate_url("https://www.youtube.com/watch?v=abc").is_ok());
    }

    #[test]
    fn test_validate_url_rejects_empty() {
        assert!(validate_url("").is_err());
    }

    #[test]
    fn test_validate_url_rejects_relative_path() {
        assert!(validate_url("/etc/passwd").is_err());
        assert!(validate_url("../../etc/passwd").is_err());
    }

    #[test]
    fn test_validate_url_rejects_other_schemes() {
        assert!(validate_url("ftp://example.com/file").is_err());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("javascript:alert(1)").is_err());
        assert!(validate_url("data:text/html,x").is_err());
    }

    #[test]
    fn test_validate_url_rejects_option_injection() {
        assert!(validate_url("--config-file=/tmp/evil").is_err());
        assert!(validate_url("-o/tmp/evil").is_err());
    }

    #[test]
    fn test_validate_url_rejects_too_long() {
        let long = format!("https://example.com/{}", "a".repeat(3000));
        assert!(validate_url(&long).is_err());
    }

    #[test]
    fn test_validate_url_accepts_at_limit() {
        let url = format!("https://example.com/{}", "a".repeat(2000));
        assert!(validate_url(&url).is_ok());
    }

    // ---------- percent_encode ----------

    #[test]
    fn test_percent_encode_unreserved_kept() {
        assert_eq!(percent_encode("AZaz09-._~"), "AZaz09-._~");
    }

    #[test]
    fn test_percent_encode_space_and_slash() {
        assert_eq!(percent_encode("hello world"), "hello%20world");
        assert_eq!(percent_encode("a/b"), "a%2Fb");
    }

    #[test]
    fn test_percent_encode_umlaut_utf8() {
        assert_eq!(percent_encode("ü"), "%C3%BC");
        assert_eq!(percent_encode("ß"), "%C3%9F");
    }

    #[test]
    fn test_percent_encode_empty() {
        assert_eq!(percent_encode(""), "");
    }

    #[test]
    fn test_percent_encode_query_safe() {
        let encoded = percent_encode("https://x.com/watch?v=1&t=2");
        assert!(!encoded.contains('&'));
        assert!(!encoded.contains('='));
        assert_eq!(encoded, "https%3A%2F%2Fx.com%2Fwatch%3Fv%3D1%26t%3D2");
    }

    // ---------- content_disposition_value ----------

    #[test]
    fn test_content_disposition_ascii_name() {
        let cd = content_disposition_value("video.mp4");
        assert!(cd.starts_with("attachment;"));
        assert!(cd.contains("filename=\"video.mp4\""));
    }

    #[test]
    fn test_content_disposition_unicode_is_header_safe() {
        let cd = content_disposition_value("Müll Video 日本語.mp4");
        assert!(
            cd.bytes().all(|b| (32..127).contains(&b)),
            "header value must be printable ASCII, got: {}",
            cd
        );
        assert!(cd.contains("filename*=UTF-8''"));
        assert!(cd.contains("%C3%BC"));
    }

    #[test]
    fn test_content_disposition_strips_quotes_and_backslash() {
        let cd = content_disposition_value("ev\"il\\name.mp4");
        let fallback = cd.split("filename=\"").nth(1).unwrap();
        let fallback = fallback.split('"').next().unwrap();
        assert!(!fallback.contains('"'));
        assert!(!fallback.contains('\\'));
    }

    #[test]
    fn test_content_disposition_only_unicode_falls_back_to_download() {
        let cd = content_disposition_value("日本語");
        assert!(cd.contains("filename=\"download\""));
    }

    #[test]
    fn test_content_disposition_preserves_full_unicode_name_in_ext() {
        let name = "Ein wirklich langes Video mit Umlauten äöü.mp4";
        let cd = content_disposition_value(name);
        assert!(cd.contains("filename*=UTF-8''"));
        let encoded = percent_encode(name);
        assert!(cd.ends_with(&encoded));
    }

    #[test]
    fn test_content_disposition_long_name_fallback_truncated() {
        let name = format!("{}.mp4", "x".repeat(500));
        let cd = content_disposition_value(&name);
        let fallback = cd.split("filename=\"").nth(1).unwrap();
        let fallback = fallback.split('"').next().unwrap();
        assert!(fallback.chars().count() <= 121);
    }

    // ---------- constant_time_eq ----------

    #[test]
    fn test_constant_time_eq_equal() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(constant_time_eq("", ""));
        assert!(constant_time_eq(
            "a".repeat(64).as_str(),
            "a".repeat(64).as_str()
        ));
    }

    #[test]
    fn test_constant_time_eq_not_equal_same_length() {
        assert!(!constant_time_eq("abc", "abd"));
    }

    #[test]
    fn test_constant_time_eq_different_lengths() {
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", "x"));
    }

    #[test]
    fn test_constant_time_eq_hash_like_inputs() {
        let a = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let b = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let c = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(constant_time_eq(a, b));
        assert!(!constant_time_eq(a, c));
    }

    // ---------- extract_command_detail ----------

    #[test]
    fn test_extract_command_detail_prefers_stderr() {
        let detail = extract_command_detail("some stdout", "ERROR: Permission denied");
        assert_eq!(detail, "ERROR: Permission denied");
    }

    #[test]
    fn test_extract_command_detail_falls_back_to_stdout_last_line() {
        let detail = extract_command_detail("line one\nlast relevant line", "");
        assert_eq!(detail, "last relevant line");
    }

    #[test]
    fn test_extract_command_detail_skips_blank_lines() {
        let detail = extract_command_detail("", "\n\n  \nreal error\n\n");
        assert_eq!(detail, "real error");
    }

    #[test]
    fn test_extract_command_detail_empty_returns_unknown() {
        assert_eq!(extract_command_detail("", ""), "unbekannter Fehler");
        assert_eq!(extract_command_detail("\n\n", "   "), "unbekannter Fehler");
    }

    #[test]
    fn test_extract_command_detail_truncates_long_lines() {
        let long = "x".repeat(500);
        let detail = extract_command_detail(&long, "");
        assert_eq!(detail.chars().count(), 200);
    }

    #[test]
    fn test_extract_command_detail_trims_whitespace() {
        assert_eq!(extract_command_detail("  spaced out  ", ""), "spaced out");
    }

    // ---------- permission problem detection ----------

    #[test]
    fn test_permission_problem_detected() {
        assert!(is_permission_problem(
            "ERROR: Unable to write to /usr/local/bin/yt-dlp; try running as administrator"
        ));
        assert!(is_permission_problem("permission denied"));
        assert!(is_permission_problem("Access Denied"));
    }

    #[test]
    fn test_permission_problem_not_confused_with_other_errors() {
        assert!(!is_permission_problem("ERROR: Unsupported URL"));
        assert!(!is_permission_problem("network timeout"));
        assert!(!is_permission_problem(""));
    }

    // ---------- share URL extraction ----------

    #[test]
    fn test_share_regex_extracts_url_from_text() {
        let text = "Schau dir das an: https://youtu.be/dQw4w9WgXcQ cool!";
        let found = SHARE_URL_REGEX.find(text).map(|m| m.as_str());
        assert_eq!(found, Some("https://youtu.be/dQw4w9WgXcQ"));
    }

    #[test]
    fn test_share_regex_takes_first_of_multiple() {
        let text = "https://a.com https://b.com";
        let found = SHARE_URL_REGEX.find(text).map(|m| m.as_str());
        assert_eq!(found, Some("https://a.com"));
    }

    #[test]
    fn test_share_regex_no_match_on_plain_text() {
        assert!(SHARE_URL_REGEX.find("nur text hier").is_none());
        assert!(SHARE_URL_REGEX.find("").is_none());
    }

    #[test]
    fn test_share_regex_stops_at_angle_brackets() {
        let text = "<https://example.com/page>";
        let found = SHARE_URL_REGEX.find(text).map(|m| m.as_str());
        assert_eq!(found, Some("https://example.com/page"));
    }

    // ---------- cache header constants ----------

    #[test]
    fn test_cache_header_values_valid_http() {
        for v in [CACHE_MANIFEST, CACHE_SW, CACHE_ICON] {
            assert!(!v.is_empty());
            let parsed: Result<axum::http::HeaderValue, _> = v.parse();
            assert!(parsed.is_ok(), "invalid cache header value: {}", v);
        }
    }
}

// ============================================================================
// Format preset grouping tests (dedup regression coverage)
// ============================================================================
#[cfg(test)]
mod format_preset_tests {
    use super::*;
    use serde_json::json;

    fn video_stream(id: &str, height: u64, ext: &str, filesize: Option<f64>) -> serde_json::Value {
        let mut v = json!({
            "format_id": id,
            "vcodec": "avc1.640028",
            "acodec": "none",
            "ext": ext,
            "height": height,
        });
        if let Some(fs) = filesize {
            v["filesize"] = json!(fs);
        }
        v
    }

    fn audio_stream(
        id: &str,
        acodec: &str,
        ext: &str,
        abr: Option<f64>,
        tbr: Option<f64>,
        filesize: Option<f64>,
    ) -> serde_json::Value {
        let mut v = json!({
            "format_id": id,
            "vcodec": "none",
            "acodec": acodec,
            "ext": ext,
        });
        if let Some(a) = abr {
            v["abr"] = json!(a);
        }
        if let Some(t) = tbr {
            v["tbr"] = json!(t);
        }
        if let Some(fs) = filesize {
            v["filesize"] = json!(fs);
        }
        v
    }

    fn info_with_formats(formats: Vec<serde_json::Value>) -> serde_json::Value {
        json!({ "id": "test", "formats": formats })
    }

    fn audio_presets(formats: &[FormatInfo]) -> Vec<&FormatInfo> {
        formats
            .iter()
            .filter(|f| f.media_type == "audio" && f.quality != "best")
            .collect()
    }

    fn video_presets(formats: &[FormatInfo]) -> Vec<&FormatInfo> {
        formats
            .iter()
            .filter(|f| f.media_type == "video" && f.quality != "best")
            .collect()
    }

    #[test]
    fn test_duplicate_opus_streams_collapse_into_one_preset() {
        // Regression: YouTube lists the same ~150 kbps Opus stream multiple times
        // (DASH/HLS duplicates with distinct format_ids). Grouping must not key
        // on format_id.
        let info = info_with_formats(vec![
            audio_stream("251", "opus", "webm", Some(150.0), None, Some(3_000_000.0)),
            audio_stream("600", "opus", "webm", Some(150.0), None, Some(3_000_000.0)),
            audio_stream("601", "opus", "webm", Some(150.4), None, Some(3_100_000.0)),
        ]);

        let presets = build_format_presets(&info);
        let audio = audio_presets(&presets);

        assert_eq!(
            audio.len(),
            1,
            "identical Opus streams must collapse into one preset, got: {:?}",
            audio.iter().map(|f| (&f.quality, &f.label)).collect::<Vec<_>>()
        );
        assert_eq!(audio[0].format, "opus");
        assert!(audio[0].label.contains("150"), "label: {}", audio[0].label);
    }

    #[test]
    fn test_near_identical_bitrates_share_a_bucket() {
        // 149 and 151 kbps round into the same ~10 kbps bucket.
        let info = info_with_formats(vec![
            audio_stream("a1", "opus", "webm", Some(149.0), None, None),
            audio_stream("b2", "opus", "webm", Some(151.0), None, None),
        ]);

        let presets = build_format_presets(&info);
        let audio = audio_presets(&presets);

        assert_eq!(audio.len(), 1);
        // The higher-scored variant wins as the representative stream.
        assert_eq!(audio[0].quality, "b2");
        assert!(audio[0].label.contains("151"));
    }

    #[test]
    fn test_best_variant_per_group_is_kept() {
        let info = info_with_formats(vec![
            audio_stream("low", "opus", "webm", Some(148.0), None, None),
            audio_stream("high", "opus", "webm", Some(152.0), None, None),
            audio_stream("mid", "opus", "webm", Some(149.5), None, None),
        ]);

        let presets = build_format_presets(&info);
        let audio = audio_presets(&presets);

        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].quality, "high");
    }

    #[test]
    fn test_distinct_codecs_and_tiers_survive_sorted_by_quality() {
        let info = info_with_formats(vec![
            audio_stream("140", "mp4a.40.2", "m4a", Some(128.0), None, None),
            audio_stream("251", "opus", "webm", Some(160.0), None, None),
            audio_stream("249", "opus", "webm", Some(70.0), None, None),
        ]);

        let presets = build_format_presets(&info);
        let audio = audio_presets(&presets);

        assert_eq!(audio.len(), 3);
        // Perceived score desc: opus@160 > m4a@128 > opus@70
        assert_eq!(audio[0].format, "opus");
        assert!(audio[0].label.contains("160"));
        assert_eq!(audio[1].format, "m4a");
        assert!(audio[1].label.contains("128"));
        assert_eq!(audio[2].format, "opus");
        assert!(audio[2].label.contains("70"));
    }

    #[test]
    fn test_tbr_fallback_used_when_abr_missing() {
        let info = info_with_formats(vec![
            audio_stream("999", "opus", "webm", None, Some(96.0), None),
        ]);

        let presets = build_format_presets(&info);
        let audio = audio_presets(&presets);

        assert_eq!(audio.len(), 1);
        assert!(audio[0].label.contains("~96 kbps"), "label: {}", audio[0].label);
    }

    #[test]
    fn test_video_streams_grouped_and_sorted_desc_by_height_then_ext() {
        let mut info = info_with_formats(vec![
            video_stream("137", 1080, "mp4", None),
            video_stream("135", 720, "mp4", None),
            video_stream("271", 1080, "webm", None),
            video_stream("137-dup", 1080, "mp4", None), // duplicate
            video_stream("313", 2160, "webm", None),
        ]);
        let mhtml = json!({
            "format_id": "sb2",
            "vcodec": "unknown",
            "acodec": "none",
            "ext": "mhtml",
            "height": 720,
        });
        if let Some(arr) = info.get_mut("formats").and_then(|f| f.as_array_mut()) {
            arr.push(mhtml);
        }

        let presets = build_format_presets(&info);
        let video = video_presets(&presets);

        let qualities: Vec<_> = video.iter().map(|f| f.quality.as_str()).collect();
        assert_eq!(
            qualities,
            vec!["2160p-webm", "1080p-mp4", "1080p-webm", "720p-mp4"]
        );
        assert!(qualities.iter().all(|q| !q.contains("mhtml")));
    }

    #[test]
    fn test_best_pseudo_entries_present_in_order() {
        let info = info_with_formats(vec![
            video_stream("137", 1080, "mp4", None),
            audio_stream("251", "opus", "webm", Some(160.0), None, None),
        ]);

        let presets = build_format_presets(&info);

        assert_eq!(presets[0].media_type, "video");
        assert_eq!(presets[0].quality, "best");
        assert_eq!(presets[0].label, "Beste Qualität (Video)");

        let audio_best_idx = presets
            .iter()
            .position(|f| f.media_type == "audio" && f.quality == "best")
            .expect("audio best entry must exist");
        assert_eq!(presets[audio_best_idx].label, "Beste Qualität (Audio)");
        // Audio best comes after all video presets.
        assert!(audio_best_idx > presets.iter().position(|f| f.media_type == "video" && f.quality != "best").unwrap());
    }

    #[test]
    fn test_filesize_lookup_uses_representative_stream() {
        let info = info_with_formats(vec![
            video_stream("137", 1080, "mp4", Some(52_428_800.0)), // 50 MB
            audio_stream("251", "opus", "webm", Some(150.0), None, Some(3_145_728.0)), // 3 MB
            audio_stream("249", "opus", "webm", Some(70.0), None, None),
        ]);

        let presets = build_format_presets(&info);

        let v1080 = presets.iter().find(|f| f.quality == "1080p-mp4").unwrap();
        assert_eq!(v1080.filesize.as_deref(), Some("50.0 MB"));

        let opus150 = presets.iter().find(|f| f.quality == "251").unwrap();
        assert_eq!(opus150.filesize.as_deref(), Some("3.0 MB"));

        let opus70 = presets.iter().find(|f| f.quality == "249").unwrap();
        assert_eq!(opus70.filesize, None);
    }

    #[test]
    fn test_missing_or_empty_formats_array_yields_only_best_entries() {
        let empty_info = json!({ "id": "x" });
        let presets = build_format_presets(&empty_info);
        assert_eq!(presets.len(), 2);
        assert!(presets.iter().all(|f| f.quality == "best"));

        let no_key = info_with_formats(vec![]);
        assert_eq!(build_format_presets(&no_key).len(), 2);
    }

    #[test]
    fn test_filesize_formatting_boundaries() {
        assert_eq!(format_filesize(-1.0), None);
        assert_eq!(format_filesize(0.0), None);
        assert_eq!(format_filesize(512.0), Some("0.5 KB".to_string()));
        assert_eq!(
            format_filesize(1_572_864.0),
            Some("1.5 MB".to_string())
        );
        assert_eq!(
            format_filesize(1024.0 * 1024.0 * 1024.0 * 2.0),
            Some("2.00 GB".to_string())
        );
    }
}
