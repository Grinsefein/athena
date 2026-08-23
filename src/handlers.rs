use axum::{
    body::Body,
    extract::{
        ConnectInfo, FromRequest, Multipart, Path, Query, Request, State,
    },
    http::{
        header::{
            self, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
        },
        StatusCode,
    },
    response::{sse::Event, Html, IntoResponse, Json, Redirect, Response, Sse},
};
use futures::stream::Stream;
use once_cell::sync::Lazy;
use regex::Regex;
use std::{collections::HashMap, io::SeekFrom, net::{IpAddr, Ipv4Addr}};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt},
    process::Command,
    time::{sleep, Duration},
};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::{
    AnalyzeResponse, ApiResponse, ConfigResponse, DownloadRequest, DownloadResponse, FormatInfo,
    LoginRequest, LoginResponse, PlaylistVideo, ProgressUpdate,
};
use crate::state::{
    consume_rate_limit, delete_download_artifacts, get_cached_meta, now_secs, store_cached_meta,
    touch_download, verify_password, ANALYZE_SEMAPHORE, AppState, DownloadInfo, SharedState,
    PASSWORD_HASH, TOKEN_MAX_AGE_SECS, API_WINDOW_SECS, DOWNLOAD_DIR, MAX_ANALYZE_PER_WINDOW,
    MAX_DOWNLOADS_PER_WINDOW,
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

/// Escape hatch for deployments that explicitly want to hand internal
/// targets to yt-dlp (e.g. testing against a local media server).
static UNSAFE_ALLOW_PRIVATE_TARGETS: Lazy<bool> = Lazy::new(|| {
    std::env::var("ATHENA_UNSAFE_ALLOW_PRIVATE_TARGETS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
});

fn invalid_url(message: &str) -> AppError {
    AppError::InvalidUrl {
        message: message.to_string(),
    }
}

/// True only for addresses that are safe to let an external fetcher dial.
///
/// Blocks loopback, RFC 1918/4193 private ranges, link-local (incl. the
/// IPv4-mapped/NAT64 embedded forms), CGNAT, benchmarking, reserved and
/// documentation ranges.
pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => {
            // ::ffff:a.b.c.d embeds a plain IPv4 address that must go through
            // the IPv4 checks; 64:ff9b::/96 does the same for NAT64.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ipv4(v4);
            }
            if v6.segments()[0] == 0x0064
                && v6.segments()[1] == 0xff9b
                && v6.segments()[2..6].iter().all(|&s| s == 0)
            {
                // Well-known NAT64 prefix 64:ff9b::/96: the last two segments
                // hold an embedded IPv4 address.
                let seg = v6.segments();
                let v4 = Ipv4Addr::new(
                    (seg[6] >> 8) as u8,
                    (seg[6] & 0xff) as u8,
                    (seg[7] >> 8) as u8,
                    (seg[7] & 0xff) as u8,
                );
                return is_public_ipv4(v4);
            }

            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (seg[0] & 0xfe00) == 0xfc00   // fc00::/7 unique local
                || (seg[0] & 0xffc0) == 0xfe80)  // fe80::/10 link-local
        }
    }
}

fn is_public_ipv4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    !(o[0] == 0                                        // "this network"
        || o[0] == 10                                  // RFC 1918
        || o[0] == 127                                 // loopback
        || (o[0] == 172 && o[1] >= 16 && o[1] <= 31)   // RFC 1918
        || (o[0] == 192 && o[1] == 168)                // RFC 1918
        || (o[0] == 169 && o[1] == 254)                // link-local
        || (o[0] == 100 && o[1] >= 64 && o[1] <= 127)  // CGNAT 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)     // 192.0.0.0/24
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking 198.18/15
        || o[0] >= 240                                 // reserved class E + broadcast
        || o == [255, 255, 255, 255])
}

/// Hostname heuristics against trivially internal names. Real protection for
/// public-looking names comes from the DNS resolution check below.
fn is_forbidden_hostname(host_lower: &str) -> bool {
    host_lower == "localhost"
        || host_lower.ends_with(".localhost")
        || host_lower.ends_with(".local")
        || host_lower.ends_with(".internal")
        || host_lower.ends_with(".lan")
        || host_lower.ends_with(".home.arpa")
        || !host_lower.contains('.')
}

async fn validate_url(url: &str) -> AppResult<()> {
    if !((url.starts_with("http://") || url.starts_with("https://")) && url.len() <= 2048) {
        return Err(invalid_url("Nur vollständige http(s)-URLs werden unterstützt"));
    }

    let parsed = url::Url::parse(url)
        .map_err(|_| invalid_url("Nur vollständige http(s)-URLs werden unterstützt"))?;

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid_url("URLs mit Zugangsdaten werden nicht unterstützt"));
    }

    if *UNSAFE_ALLOW_PRIVATE_TARGETS {
        return Ok(());
    }

    let host = parsed
        .host_str()
        .filter(|h| !h.is_empty())
        // url::Url serializes IPv6 hosts with brackets ("[::1]")
        .map(|h| h.trim_start_matches('[').trim_end_matches(']'))
        .filter(|h| !h.is_empty())
        .ok_or_else(|| invalid_url("URL enthält keinen Host"))?;

    // Literal IP addresses are checked directly ...
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(invalid_url(
                "Zugriff auf lokale oder interne Adressen ist nicht erlaubt",
            ));
        }
        return Ok(());
    }

    // ... everything else must at least look public and resolve to
    // exclusively public addresses.
    let host_lower = host.to_ascii_lowercase();
    if is_forbidden_hostname(&host_lower) {
        return Err(invalid_url(
            "Zugriff auf lokale oder interne Adressen ist nicht erlaubt",
        ));
    }

    let port = parsed.port_or_known_default().unwrap_or(80);
    let resolved = tokio::net::lookup_host((host_lower.as_str(), port))
        .await
        .map_err(|_| invalid_url(&format!("Host '{host}' konnte nicht aufgelöst werden")))?;

    if resolved.map(|addr| addr.ip()).any(|ip| !is_public_ip(ip)) {
        return Err(invalid_url(
            "Zieladresse verweist auf eine lokale oder interne Adresse",
        ));
    }

    Ok(())
}

/// Per-IP sliding-window rate limit for the expensive API endpoints
/// (/api/analyze, /api/download). Complements the global concurrency
/// semaphores, which alone cannot stop a single client from flooding the
/// queue when no password is configured.
async fn enforce_api_rate_limit(
    state: &AppState,
    endpoint: &str,
    ip: std::net::IpAddr,
) -> AppResult<()> {
    let max = match endpoint {
        "analyze" => *MAX_ANALYZE_PER_WINDOW,
        "download" => *MAX_DOWNLOADS_PER_WINDOW,
        _ => unreachable!("unknown rate-limit endpoint"),
    };

    if consume_rate_limit(&state.api_rate_limits, &format!("{endpoint}:{ip}"), max, API_WINDOW_SECS)
        .await
    {
        Ok(())
    } else {
        warn!(
            "Rate limited {endpoint} request from {ip} (limit: {max}/{})",
            API_WINDOW_SECS as u64
        );
        Err(AppError::TooManyRequests {
            message: "Zu viele Anfragen. Bitte einen Moment warten.".to_string(),
        })
    }
}

/// Reduce a URL to a stable, tracking-free canonical form so that the same
/// video/playlist always maps to the same metadata-cache key.
///
/// - youtu.be/<id>            -> https://www.youtube.com/watch?v=<id>
/// - youtube.com/watch?v=<id> -> query reduced to just v= (list=, si=, is=, ... dropped)
/// - youtube.com/playlist?list=<id> -> query reduced to just list=
/// - any other host or shape is returned unchanged
pub fn canonicalize_url(raw: &str) -> String {
    let trimmed = raw.trim();
    let rest = match trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
    {
        Some(r) => r,
        None => return trimmed.to_string(),
    };

    let (before_query, query) = match rest.find('?') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let (host_raw, path) = match before_query.find('/') {
        Some(i) => (&before_query[..i], &before_query[i..]),
        None => (before_query, ""),
    };
    let host = host_raw.to_ascii_lowercase();

    let get_param = |name: &str| -> Option<&str> {
        query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == name && !v.is_empty()).then_some(v)
        })
    };

    let is_youtube = host == "youtu.be"
        || host == "youtube.com"
        || host == "m.youtube.com"
        || host == "music.youtube.com"
        || host.ends_with(".youtube.com");
    if !is_youtube {
        return trimmed.to_string();
    }

    if host == "youtu.be" {
        let id = path.trim_start_matches('/').split('/').next().unwrap_or("");
        if id.is_empty() {
            return trimmed.to_string();
        }
        return format!("https://www.youtube.com/watch?v={}", id);
    }

    match path {
        "/watch" => match get_param("v") {
            Some(id) => format!("https://www.youtube.com/watch?v={}", id),
            None => trimmed.to_string(),
        },
        "/playlist" => match get_param("list") {
            Some(l) => format!("https://www.youtube.com/playlist?list={}", l),
            None => trimmed.to_string(),
        },
        p if p.starts_with("/shorts/") => {
            let id = p.trim_start_matches("/shorts/").split('/').next().unwrap_or("");
            if id.is_empty() {
                trimmed.to_string()
            } else {
                format!("https://www.youtube.com/watch?v={}", id)
            }
        }
        _ => trimmed.to_string(),
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

/// Web Share Target entry point.
///
/// Android sends the shared fields either as `multipart/form-data` or as
/// `application/x-www-form-urlencoded` (those are the only two encodings
/// allowed by the Web Share Target spec), so both must be understood here.
pub async fn handle_share(request: axum::extract::Request) -> Redirect {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let (url, text) = if content_type.starts_with("multipart/form-data") {
        extract_share_fields_multipart(request).await
    } else {
        extract_share_fields_urlencoded(request).await
    };

    let candidate = url.or_else(|| {
        text.as_deref()
            .and_then(|text| SHARE_URL_REGEX.find(text).map(|m| m.as_str().to_string()))
    });

    match candidate.filter(|u| u.starts_with("http://") || u.starts_with("https://")) {
        Some(url) => Redirect::to(&format!("/?share={}", percent_encode(&url))),
        None => Redirect::to("/"),
    }
}

async fn extract_share_fields_multipart(
    request: Request,
) -> (Option<String>, Option<String>) {
    let mut multipart = match Multipart::from_request(request, &()).await {
        Ok(m) => m,
        Err(_) => return (None, None),
    };

    let mut url = None;
    let mut text = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        let value = match field.text().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        match name.as_str() {
            "url" if url.is_none() => url = Some(value),
            "text" if text.is_none() => text = Some(value),
            _ => {}
        }
    }
    (url, text)
}

async fn extract_share_fields_urlencoded(
    request: Request,
) -> (Option<String>, Option<String>) {
    const MAX_FORM_BYTES: usize = 64 * 1024;
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_FORM_BYTES).await {
        Ok(b) => b,
        Err(_) => return (None, None),
    };

    let mut url = None;
    let mut text = None;
    for (key, value) in form_urlencoded::parse(&bytes) {
        match key.as_ref() {
            "url" if url.is_none() => url = Some(value.into_owned()),
            "text" if text.is_none() => text = Some(value.into_owned()),
            _ => {}
        }
    }
    (url, text)
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

    if PASSWORD_HASH.is_some() {
        if verify_password(&request.password) {
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

/// Invalidate the presented auth token (logout / device loss).
///
/// Idempotent by design: an unknown or already-expired token still answers
/// with success so clients can clear their local state either way.
pub async fn logout(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Json<ApiResponse<serde_json::Value>> {
    let query_token = params.get("token").map(|s| s.as_str());
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .or(query_token);

    let removed = match token.filter(|t| !t.is_empty()) {
        Some(t) => state.auth_tokens.lock().await.remove(t).is_some(),
        None => false,
    };

    info!(removed, "Logout processed");
    Json(ApiResponse {
        success: true,
        data: Some(serde_json::json!({ "logged_out": removed })),
        error: None,
    })
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
        format: "mp3".to_string(),
        quality: "best".to_string(),
        label: "Beste Qualität (MP3)".to_string(),
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

fn build_analyze_response(info: &serde_json::Value, canonical_url: &str) -> AnalyzeResponse {
    let duration_secs = info.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0) as u64;
    let duration_mins = duration_secs / 60;
    let duration_rem = duration_secs % 60;
    let duration = format!("{}:{:02}", duration_mins, duration_rem);

    let description = info
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect::<String>();

    AnalyzeResponse {
        id: info
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("unknown")
            .to_string(),
        url: canonical_url.to_string(),
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
        formats: build_format_presets(info),
        playlist_videos: None,
    }
}

pub async fn analyze_video(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(request): Json<DownloadRequest>,
) -> AppResult<Json<ApiResponse<AnalyzeResponse>>> {
    if !is_authenticated(&headers, None, &state).await {
        return Err(AppError::Unauthorized {
            message: "Unauthorized".to_string(),
        });
    }

    enforce_api_rate_limit(&state, "analyze", addr.ip()).await?;

    validate_url(&request.url).await?;
    let canonical_url = canonicalize_url(&request.url);
    info!("Analyzing video/playlist: {}", request.url);

    if let Some(info) = get_cached_meta(&state, &canonical_url).await {
        info!("Metadata cache hit for {}", canonical_url);
        return Ok(Json(ApiResponse {
            success: true,
            data: Some(build_analyze_response(&info, &canonical_url)),
            error: None,
        }));
    }

    let _permit = ANALYZE_SEMAPHORE.acquire().await.map_err(|_| {
        AppError::Internal {
            message: "Analyze-Semaphore wurde geschlossen".to_string(),
        }
    })?;

    let flat_output = tokio::time::timeout(
        Duration::from_secs(45),
        Command::new("yt-dlp")
            .args([
                "--quiet",
                "--no-warnings",
                "--flat-playlist",
                "--dump-single-json",
                &canonical_url,
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
            url: canonical_url.clone(),
            title: playlist_title,
            description: format!(
                "Playlist mit {} Videos",
                playlist_videos.as_ref().map(|v| v.len()).unwrap_or(0)
            ),
            thumbnail: "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%23434ed1'%3E%3Cpath d='M4 6H2v14c0 1.1.9 2 2 2h14v-2H4V6zm16-4H8c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm-4 12.5v-9l-6 4.5 6 4.5z'/%3E%3C/svg%3E".to_string(),
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
                    format: "mp3".to_string(),
                    quality: "best".to_string(),
                    label: "Beste Qualität (MP3)".to_string(),
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
                &canonical_url,
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

    let response = build_analyze_response(&info, &canonical_url);
    store_cached_meta(&state, canonical_url, info).await;

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
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(request): Json<DownloadRequest>,
) -> AppResult<Json<ApiResponse<DownloadResponse>>> {
    if !is_authenticated(&headers, None, &state).await {
        return Err(AppError::Unauthorized {
            message: "Unauthorized".to_string(),
        });
    }

    enforce_api_rate_limit(&state, "download", addr.ip()).await?;

    validate_url(&request.url).await?;
    let canonical_url = canonicalize_url(&request.url);
    if let Some(urls) = &request.playlist_urls {
        for url in urls {
            validate_url(url).await?;
        }
    }

    let download_id = Uuid::new_v4().to_string()[..8].to_string();
    info!("Starting download {} for URL: {}", download_id, canonical_url);

    {
        let mut downloads = state.active_downloads.lock().await;
        let conflict = downloads.values().any(|info| {
            (info.status == "queued" || info.status == "processing")
                && info.url.as_deref() == Some(canonical_url.as_str())
        });
        if conflict {
            return Ok(Json(ApiResponse {
                success: false,
                data: None,
                error: Some(
                    "Für dieses Video läuft bereits ein Download. \
                     Bitte warten oder den laufenden Download abbrechen."
                        .to_string(),
                ),
            }));
        }

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
                last_activity: now_secs(),
                url: Some(canonical_url.clone()),
                lyrics_plain: None,
                lyrics_synced: None,
            },
        );
    }

    let want_lyrics = request.lyrics;
    let state_clone = state.clone();
    let download_id_clone = download_id.clone();
    let canonical_for_task = canonical_url;
    tokio::spawn(async move {
        if let Some(urls) = request.playlist_urls {
            if !urls.is_empty() {
                playlist_download_task(
                    state_clone,
                    download_id_clone,
                    urls,
                    request.format,
                    request.quality,
                    want_lyrics,
                )
                .await;
                return;
            }
        }

        download_task(
            state_clone,
            download_id_clone,
            canonical_for_task,
            request.format,
            request.quality,
            want_lyrics,
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

/// Remove partial download artifacts belonging to an aborted download:
/// yt-dlp `.part`/`.ytdl` fragments and the playlist temp directory.
async fn cleanup_partial_files(download_id: &str) {
    let id_marker = format!("-[{}].", download_id);
    if let Ok(mut entries) = fs::read_dir(&*DOWNLOAD_DIR).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if name.contains(&id_marker) && (name.ends_with(".part") || name.ends_with(".ytdl")) {
                if let Err(e) = fs::remove_file(&path).await {
                    warn!("Abort cleanup: failed to remove {:?}: {}", path, e);
                } else {
                    info!("Abort cleanup: removed partial file {:?}", path);
                }
            }
        }
    }

    let temp_dir = DOWNLOAD_DIR.join(format!("playlist-temp-{}", download_id));
    if temp_dir.exists() {
        if let Err(e) = fs::remove_dir_all(&temp_dir).await {
            warn!("Abort cleanup: failed to remove {:?}: {}", temp_dir, e);
        } else {
            info!("Abort cleanup: removed playlist temp dir {:?}", temp_dir);
        }
    }
}

pub async fn abort_download(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(download_id): Path<String>,
) -> AppResult<Json<ApiResponse<serde_json::Value>>> {
    if !is_authenticated(&headers, None, &state).await {
        return Err(AppError::Unauthorized {
            message: "Unauthorized".to_string(),
        });
    }

    // 1. Kill all yt-dlp child processes of this download. Single downloads are
    //    registered under the plain id, playlist videos under "<id>-<index>".
    let mut killed = 0usize;
    {
        let mut children = state.active_children.lock().await;
        let prefix = format!("{}-", download_id);
        let ids: Vec<String> = children
            .keys()
            .filter(|k| *k == &download_id || k.starts_with(&prefix))
            .cloned()
            .collect();
        for id in ids {
            if let Some(mut child) = children.remove(&id) {
                let _ = child.kill().await;
                killed += 1;
            }
        }
    }
    info!("Aborting download {}: killed {} process(es)", download_id, killed);

    // 2. Mark as aborted so the SSE stream notifies clients and the task's own
    //    error handling does not overwrite the status afterwards.
    let was_running = {
        let mut downloads = state.active_downloads.lock().await;
        match downloads.get_mut(&download_id) {
            Some(info) if info.status == "queued" || info.status == "processing" => {
                info.status = "aborted".to_string();
                info.speed = None;
                info.eta = None;
                true
            }
            _ => false,
        }
    };

    if !was_running {
        return Ok(Json(ApiResponse {
            success: false,
            data: None,
            error: Some("Download läuft nicht mehr".to_string()),
        }));
    }

    cleanup_partial_files(&download_id).await;

    Ok(Json(ApiResponse {
        success: true,
        data: Some(serde_json::json!({ "status": "aborted" })),
        error: None,
    }))
}

pub async fn progress_stream(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(download_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let query_token = params.get("token").map(|s| s.as_str());
    let authenticated = is_authenticated(&headers, query_token, &state).await;

    let stream: std::pin::Pin<
        Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>,
    > = Box::pin(async_stream::stream! {
        if !authenticated {
            let error_json = serde_json::to_string(&ProgressUpdate {
                status: "error".to_string(),
                progress: 0.0,
                download_url: None,
                error: Some("Unauthorized".to_string()),
                speed: None,
                eta: None,
                lyrics_plain: None,
                lyrics_synced: None,
            }).unwrap_or_default();
            yield Ok(Event::default().data(error_json));
            return;
        }

        loop {
            let (status, progress, file_path, error, speed, eta, lyrics_plain, lyrics_synced) = {
                let downloads = state.active_downloads.lock().await;
                match downloads.get(&download_id) {
                    Some(info) => (
                        info.status.clone(),
                        info.progress,
                        info.file_path.clone(),
                        info.error.clone(),
                        info.speed.clone(),
                        info.eta.clone(),
                        info.lyrics_plain.clone(),
                        info.lyrics_synced.clone(),
                    ),
                    None => {
                        let json = serde_json::to_string(&ProgressUpdate {
                            status: String::from("error"),
                            progress: 0.0,
                            download_url: None,
                            error: Some(String::from("Download not found")),
                            speed: None,
                            eta: None,
                            lyrics_plain: None,
                            lyrics_synced: None,
                        }).unwrap_or_default();
                        yield Ok(Event::default().data(json));
                        break;
                    }
                }
            };

            let should_break =
                status == "completed" || status == "error" || status == "aborted";
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
                lyrics_plain,
                lyrics_synced,
            }).unwrap_or_default();
            yield Ok(Event::default().data(json));

            if should_break {
                break;
            }

            sleep(Duration::from_millis(500)).await;
        }
    });

    let mut sse_response = Sse::new(stream).into_response();
    let response_headers = sse_response.headers_mut();
    // Reverse proxies (nginx & co.) buffer SSE by default, which delays every
    // progress update until the stream ends — exactly the "stuck in queue"
    // symptom. These headers disable that buffering end-to-end.
    response_headers.insert("x-accel-buffering", "no".parse().unwrap());
    response_headers.insert(CACHE_CONTROL, "no-cache".parse().unwrap());
    sse_response
}

/// Outcome of parsing a `Range` header against a concrete file size.
#[derive(Debug, PartialEq)]
enum ParsedRange {
    /// Serve the whole file (no header, malformed syntax, or multi-range).
    Full,
    /// `bytes=start-end` (inclusive), already clamped to the file size.
    Partial { start: u64, length: u64 },
    /// Syntactically valid but outside the file -> answer with 416.
    Unsatisfiable,
}

/// Parse a single-range `Range` header (RFC 9110 §14) against `size`.
///
/// Malformed headers are ignored (serve the full file) as mandated by the
/// spec; multi-range requests are deliberately not supported and also fall
/// back to the full response.
fn parse_range_header(range: Option<&str>, size: u64) -> ParsedRange {
    let Some(spec) = range.and_then(|r| r.strip_prefix("bytes=")) else {
        return ParsedRange::Full;
    };

    if spec.contains(',') {
        return ParsedRange::Full;
    }

    let Some((start_raw, end_raw)) = spec.trim().split_once('-') else {
        return ParsedRange::Full;
    };
    let start_raw = start_raw.trim();
    let end_raw = end_raw.trim();

    if start_raw.is_empty() {
        // Suffix form: last N bytes ("bytes=-500")
        let Ok(suffix_len) = end_raw.parse::<u64>() else {
            return ParsedRange::Full;
        };
        if suffix_len == 0 || size == 0 {
            return ParsedRange::Unsatisfiable;
        }
        let start = size.saturating_sub(suffix_len);
        return ParsedRange::Partial {
            start,
            length: size - start,
        };
    }

    let Ok(start) = start_raw.parse::<u64>() else {
        return ParsedRange::Full;
    };
    if start >= size {
        return ParsedRange::Unsatisfiable;
    }

    let end = if end_raw.is_empty() {
        size - 1
    } else {
        let Ok(end) = end_raw.parse::<u64>() else {
            return ParsedRange::Full;
        };
        if end < start {
            return ParsedRange::Full;
        }
        end.min(size - 1)
    };

    ParsedRange::Partial {
        start,
        length: end - start + 1,
    }
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

    // Serving the file is a sign of life for the owning session and keeps the
    // retention timer running; the file stays available for repeat saves or
    // other formats until it expires (see FILE_RETENTION_MINUTES).
    touch_download(&state, &download_id).await;

    let file_size = fs::metadata(&file_path).await?.len();

    // Resume support: honor a single-range Range header so interrupted
    // transfers can continue instead of restarting (mobile networks!).
    let parsed_range = parse_range_header(
        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
        file_size,
    );

    let (status, start, length) = match parsed_range {
        ParsedRange::Full => (StatusCode::OK, 0, file_size),
        ParsedRange::Partial { start, length } => {
            info!(
                "Partial download request for {} ({}-{} of {} bytes)",
                download_id,
                start,
                start + length - 1,
                file_size
            );
            (StatusCode::PARTIAL_CONTENT, start, length)
        }
        ParsedRange::Unsatisfiable => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(CONTENT_RANGE, format!("bytes */{file_size}"))],
            )
                .into_response());
        }
    };

    let mut file = tokio::fs::File::open(&file_path).await?;
    if start > 0 {
        file.seek(SeekFrom::Start(start)).await?;
    }
    let stream = tokio_util::io::ReaderStream::new(file.take(length));
    let body = Body::from_stream(stream);

    let disposition = content_disposition_value(&clean_name);

    let mut response = (status, body).into_response();
    let response_headers = response.headers_mut();
    response_headers.insert(CONTENT_TYPE, "application/octet-stream".parse().unwrap());
    response_headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    response_headers.insert(CONTENT_LENGTH, length.to_string().parse().unwrap());
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            CONTENT_RANGE,
            format!("bytes {}-{}/{file_size}", start, start + length - 1)
                .parse()
                .unwrap(),
        );
    }
    response_headers.insert(
        "content-disposition",
        disposition.parse().map_err(|_| AppError::Internal {
            message: "Invalid content disposition".to_string(),
        })?,
    );
    response_headers.insert("x-content-type-options", "nosniff".parse().unwrap());

    Ok(response)
}

/// Session keep-alive: refreshes the retention timer of a download without
/// transferring anything. Unknown ids answer with success=false so the client
/// can silently drop stale state.
pub async fn heartbeat_download(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(download_id): Path<String>,
) -> AppResult<Json<ApiResponse<serde_json::Value>>> {
    if !is_authenticated(&headers, None, &state).await {
        return Err(AppError::Unauthorized {
            message: "Unauthorized".to_string(),
        });
    }

    let alive = touch_download(&state, &download_id).await;
    Ok(Json(ApiResponse {
        success: true,
        data: Some(serde_json::json!({ "alive": alive })),
        error: None,
    }))
}

/// Explicitly discard a finished download (new video = new session). Active
/// downloads are refused; use /api/abort for those.
pub async fn release_download(
    State(state): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(download_id): Path<String>,
) -> AppResult<Json<ApiResponse<serde_json::Value>>> {
    if !is_authenticated(&headers, None, &state).await {
        return Err(AppError::Unauthorized {
            message: "Unauthorized".to_string(),
        });
    }

    {
        let downloads = state.active_downloads.lock().await;
        match downloads.get(&download_id) {
            Some(info) if info.status == "queued" || info.status == "processing" => {
                return Ok(Json(ApiResponse {
                    success: false,
                    data: None,
                    error: Some("Download läuft noch".to_string()),
                }));
            }
            _ => {}
        }
    }

    let removed = delete_download_artifacts(&state, &download_id).await;
    info!(
        "Released download {}: {}",
        download_id,
        if removed { "removed" } else { "not found" }
    );

    Ok(Json(ApiResponse {
        success: true,
        data: Some(serde_json::json!({ "released": removed })),
        error: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- validate_url ----------

    #[tokio::test]
    async fn test_validate_url_accepts_http() {
        assert!(validate_url("http://93.184.216.34").await.is_ok());
    }

    #[tokio::test]
    async fn test_validate_url_accepts_https() {
        // Literal public IP keeps this test free of DNS lookups
        assert!(validate_url("https://93.184.216.34/watch?v=abc")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_validate_url_rejects_empty() {
        assert!(validate_url("").await.is_err());
    }

    #[tokio::test]
    async fn test_validate_url_rejects_relative_path() {
        assert!(validate_url("/etc/passwd").await.is_err());
        assert!(validate_url("../../etc/passwd").await.is_err());
    }

    #[tokio::test]
    async fn test_validate_url_rejects_other_schemes() {
        for url in [
            "ftp://example.com/file",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
        ] {
            assert!(
                validate_url(url).await.is_err(),
                "{url} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn test_validate_url_rejects_option_injection() {
        assert!(validate_url("--config-file=/tmp/evil").await.is_err());
        assert!(validate_url("-o/tmp/evil").await.is_err());
    }

    #[tokio::test]
    async fn test_validate_url_rejects_too_long() {
        let long = format!("https://93.184.216.34/{}", "a".repeat(3000));
        assert!(validate_url(&long).await.is_err());
    }

    #[tokio::test]
    async fn test_validate_url_accepts_at_limit() {
        let url = format!("https://93.184.216.34/{}", "a".repeat(2000));
        assert!(validate_url(&url).await.is_ok());
    }

    // ---------- validate_url: SSRF protection ----------

    #[tokio::test]
    async fn test_validate_url_rejects_private_ipv4_targets() {
        for host in [
            "http://127.0.0.1/",
            "http://10.0.0.5/",
            "http://172.16.1.1/",
            "http://192.168.178.40/",
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://100.64.0.1/",                       // CGNAT
            "http://0.0.0.0/",
        ] {
            assert!(
                validate_url(host).await.is_err(),
                "{host} must be rejected as internal target"
            );
        }
    }

    #[tokio::test]
    async fn test_validate_url_rejects_internal_ipv6_targets() {
        for host in [
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fc00::1]/",
            "http://[fd12:3456::1]/",
            "http://[::ffff:192.168.1.1]/",  // IPv4-mapped private
            "http://[::ffff:127.0.0.1]/",    // IPv4-mapped loopback
            "http://[64:ff9b::c0a8:102]/",   // NAT64-mapped 192.168.1.2
        ] {
            assert!(
                validate_url(host).await.is_err(),
                "{host} must be rejected as internal target"
            );
        }
    }

    #[tokio::test]
    async fn test_validate_url_rejects_localhost_and_intranet_names() {
        for host in [
            "http://localhost/",
            "http://localhost:8000/",
            "http://pi.hole.local/",
            "http://fritz.box.internal/",
            "http://nas.lan/",
            "http://intranet.home.arpa/",
            "http://myserver/", // dotless intranet name
        ] {
            assert!(
                validate_url(host).await.is_err(),
                "{host} must be rejected as internal-looking hostname"
            );
        }
    }

    #[tokio::test]
    async fn test_validate_url_rejects_embedded_credentials() {
        assert!(validate_url("http://user:pass@93.184.216.34/")
            .await
            .is_err());
        assert!(validate_url("http://admin@192.168.0.1/admin").await.is_err());
    }

    #[tokio::test]
    async fn test_validate_url_public_ip_still_allowed() {
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(142, 250, 74, 110))));
        assert!(is_public_ip(IpAddr::V6(
            "2606:4700::6810:85e5".parse().unwrap()
        )));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(!is_public_ip(IpAddr::V6("::1".parse().unwrap())));
    }

    // ---------- parse_range_header ----------

    #[test]
    fn test_range_no_header_serves_full_file() {
        assert_eq!(
            parse_range_header(None, 1000),
            ParsedRange::Full,
            "missing Range header must serve the whole file"
        );
        assert_eq!(
            parse_range_header(Some(""), 1000),
            ParsedRange::Full,
            "empty Range header must serve the whole file"
        );
        assert_eq!(
            parse_range_header(Some("items=0-99"), 1000),
            ParsedRange::Full,
            "non-bytes units are ignored"
        );
    }

    #[test]
    fn test_range_malformed_headers_are_ignored() {
        for header in ["bytes=", "bytes=abc", "bytes=50-20", "bytes=1-2,3-4"] {
            assert_eq!(
                parse_range_header(Some(header), 1000),
                ParsedRange::Full,
                "malformed Range '{header}' must serve the full file"
            );
        }
    }

    #[test]
    fn test_range_simple_partial_requests() {
        assert_eq!(
            parse_range_header(Some("bytes=0-99"), 1000),
            ParsedRange::Partial { start: 0, length: 100 }
        );
        assert_eq!(
            parse_range_header(Some("bytes=100-"), 1000),
            ParsedRange::Partial { start: 100, length: 900 }
        );
        // end beyond file size gets clamped
        assert_eq!(
            parse_range_header(Some("bytes=900-999999"), 1000),
            ParsedRange::Partial { start: 900, length: 100 }
        );
    }

    #[test]
    fn test_range_suffix_requests() {
        assert_eq!(
            parse_range_header(Some("bytes=-500"), 1000),
            ParsedRange::Partial { start: 500, length: 500 }
        );
        // suffix longer than the file serves everything
        assert_eq!(
            parse_range_header(Some("bytes=-5000"), 1000),
            ParsedRange::Partial { start: 0, length: 1000 }
        );
    }

    #[test]
    fn test_range_unsatisfiable_requests() {
        assert_eq!(
            parse_range_header(Some("bytes=1000-2000"), 1000),
            ParsedRange::Unsatisfiable
        );
        assert_eq!(
            parse_range_header(Some("bytes=-0"), 1000),
            ParsedRange::Unsatisfiable
        );
        // empty file: every range is unsatisfiable
        assert_eq!(
            parse_range_header(Some("bytes=0-"), 0),
            ParsedRange::Unsatisfiable
        );
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

    // ---------- constant_time_eq (re-exported from state) ----------
    use crate::state::constant_time_eq;

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
// canonicalize_url tests
// ============================================================================
#[cfg(test)]
mod canonicalize_url_tests {
    use super::*;

    #[test]
    fn test_youtu_be_with_tracking_param() {
        assert_eq!(
            canonicalize_url("https://youtu.be/T_GhB7lK2YE?is=IVSwoUOiEa_mhpa0"),
            "https://www.youtube.com/watch?v=T_GhB7lK2YE"
        );
    }

    #[test]
    fn test_watch_url_strips_all_extra_params() {
        assert_eq!(
            canonicalize_url(
                "https://www.youtube.com/watch?v=jv3nntYylpc&is=LvPdyOZeeFY5Ue5w&t=42s&si=x"
            ),
            "https://www.youtube.com/watch?v=jv3nntYylpc"
        );
    }

    #[test]
    fn test_playlist_strips_tracking() {
        assert_eq!(
            canonicalize_url(
                "https://youtube.com/playlist?list=OLAK5uy_m9ZGs69s2Xh3UKl4ymQeRGTDT51_s1Aoc&si=5Z6WtwnIFu3E4bFZ"
            ),
            "https://www.youtube.com/playlist?list=OLAK5uy_m9ZGs69s2Xh3UKl4ymQeRGTDT51_s1Aoc"
        );
    }

    #[test]
    fn test_mobile_and_music_hosts_map_to_watch() {
        assert_eq!(
            canonicalize_url("https://m.youtube.com/watch?v=abc123XYZ_-"),
            "https://www.youtube.com/watch?v=abc123XYZ_-"
        );
        assert_eq!(
            canonicalize_url("https://music.youtube.com/watch?v=abc123XYZ_-&list=PLxyz"),
            "https://www.youtube.com/watch?v=abc123XYZ_-"
        );
    }

    #[test]
    fn test_shorts_url_maps_to_watch() {
        assert_eq!(
            canonicalize_url("https://www.youtube.com/shorts/dQw4w9WgXcQ?feature=share"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }

    #[test]
    fn test_no_query_string_variants() {
        assert_eq!(
            canonicalize_url("https://youtu.be/dQw4w9WgXcQ"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
        assert_eq!(
            canonicalize_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }

    #[test]
    fn test_same_video_different_inputs_share_key() {
        let a = canonicalize_url("https://youtu.be/abc123XYZ_-?si=t1");
        let b = canonicalize_url("https://m.youtube.com/watch?t=5&v=abc123XYZ_-");
        let c = canonicalize_url("http://www.youtube.com/watch?v=abc123XYZ_-&pp=ygE");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn test_non_youtube_urls_pass_through() {
        assert_eq!(canonicalize_url("https://vimeo.com/12345"), "https://vimeo.com/12345");
        assert_eq!(canonicalize_url("https://youtu.be.fake/x?y=z"), "https://youtu.be.fake/x?y=z");
    }

    #[test]
    fn test_malformed_input_returns_trimmed_original() {
        assert_eq!(canonicalize_url("  not a url  "), "not a url");
        assert_eq!(canonicalize_url(""), "");
    }

    #[test]
    fn test_empty_or_missing_ids_return_original() {
        assert_eq!(
            canonicalize_url("https://www.youtube.com/watch?list=only_list"),
            "https://www.youtube.com/watch?list=only_list"
        );
        assert_eq!(
            canonicalize_url("https://www.youtube.com/playlist?si=no_list"),
            "https://www.youtube.com/playlist?si=no_list"
        );
        assert_eq!(
            canonicalize_url("https://youtu.be/"),
            "https://youtu.be/"
        );
    }

    #[test]
    fn test_whitespace_is_trimmed() {
        assert_eq!(
            canonicalize_url("  https://youtu.be/abc123XYZ_- \n"),
            "https://www.youtube.com/watch?v=abc123XYZ_-"
        );
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
        assert_eq!(presets[audio_best_idx].label, "Beste Qualität (MP3)");
        assert_eq!(presets[audio_best_idx].format, "mp3");
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
