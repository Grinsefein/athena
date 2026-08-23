//! Handler-level tests exercising the HTTP handlers directly.
//!
//! These tests never spawn yt-dlp and never touch the network: they call the
//! handler functions with constructed extractors, or trigger paths that fail
//! validation before any subprocess would be launched.

use athena::handlers;
use athena::models::{ApiResponse, ConfigResponse, DownloadRequest, LoginRequest};
use athena::state::{
    now_secs, AppState, SharedState, TOKEN_MAX_AGE_SECS, MAX_ANALYZE_PER_WINDOW,
    MAX_DOWNLOADS_PER_WINDOW,
};
use axum::body::{Body, HttpBody};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use futures::StreamExt;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

fn fresh_state() -> SharedState {
    Arc::new(AppState {
        active_downloads: Mutex::new(HashMap::new()),
        auth_tokens: Mutex::new(HashMap::new()),
        active_children: Mutex::new(HashMap::new()),
        login_attempts: Mutex::new(HashMap::new()),
        api_rate_limits: Mutex::new(HashMap::new()),
        metadata_cache: Mutex::new(HashMap::new()),
    })
}

fn auth_enabled() -> bool {
    athena::state::PASSWORD_HASH.is_some()
}

fn bearer_headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
    );
    headers
}

async fn register_token(state: &SharedState, token: &str) {
    let mut tokens = state.auth_tokens.lock().await;
    tokens.insert(token.to_string(), now_secs());
}

// ============================================================================
// Static assets
// ============================================================================

#[tokio::test]
async fn root_serves_embedded_frontend() {
    let html = handlers::root().await.0;

    assert!(html.contains("<title>KlauTube</title>"), "title tag present");
    assert!(html.contains("id=\"app\""), "Svelte app mount container");
    assert!(html.contains("results-mode"), "mobile results mode styling");
    assert!(html.contains("qualitySelect"), "quality select control");
    assert!(html.contains("dl-wrap"), "sticky download wrapper");
}

#[tokio::test]
async fn frontend_redesign_regressions_are_absent() {
    let html = handlers::root().await.0;

    assert!(!html.contains("quality-grid"), "grid was replaced by select");
    assert!(!html.contains("q-card"), "grid cards were removed");
    assert!(!html.contains("best-stamp"), "old stamp marker was removed");
    assert!(!html.contains("expand-btn"), "expander was removed");
    assert!(
        html.contains(".shell.results-mode .footer"),
        "footer must be hidden in results view via CSS"
    );
}

#[tokio::test]
async fn manifest_served_as_json_with_cache_header() {
    let response = handlers::manifest_handler().await.into_response();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/json"
    );
    assert!(response.headers().contains_key(header::CACHE_CONTROL));
}

#[tokio::test]
async fn service_worker_served_as_javascript_no_cache() {
    let response = handlers::sw_handler().await.into_response();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/javascript"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
}

#[tokio::test]
async fn icon_served_as_png() {
    let response = handlers::icon_handler().await.into_response();

    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert!(
        response.body().size_hint().exact().unwrap_or(0) > 0,
        "icon bytes must be embedded"
    );
}

// ============================================================================
// Config
// ============================================================================

#[tokio::test]
async fn config_reports_success_and_auth_state() {
    let Json(ApiResponse { success, data, .. }) = handlers::get_config().await;

    assert!(success);
    let data: ConfigResponse = data.expect("config data present");
    assert_eq!(data.auth_enabled, auth_enabled());
}

// ============================================================================
// Share endpoint (redirect logic)
//
// Android delivers Web Share Target posts either as multipart/form-data or
// application/x-www-form-urlencoded, so both encodings are exercised here.
// ============================================================================

fn share_request_multipart(fields: &[(&str, &str)]) -> Request {
    const BOUNDARY: &str = "athena-test-boundary";
    let mut body = String::new();
    for (name, value) in fields {
        body.push_str(&format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        ));
    }
    body.push_str(&format!("--{BOUNDARY}--\r\n"));

    Request::builder()
        .method("POST")
        .uri("/share")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap()
}

fn share_request_urlencoded(fields: &[(&str, &str)]) -> Request {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    for (name, value) in fields {
        serializer.append_pair(name, value);
    }

    Request::builder()
        .method("POST")
        .uri("/share")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(serializer.finish()))
        .unwrap()
}

fn location_of(response: axum::response::Response) -> String {
    response
        .headers()
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn share_redirects_with_url_param() {
    let response = handlers::handle_share(share_request_multipart(&[
        ("title", "A video"),
        ("url", "https://youtu.be/dQw4w9WgXcQ"),
    ]))
    .await
    .into_response();

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = location_of(response);
    assert!(location.starts_with("/?share=https%3A%2F%2Fyoutu.be%2F"));
}

#[tokio::test]
async fn share_extracts_first_url_from_text_in_both_encodings() {
    for request in [
        share_request_multipart(&[("text", "Check this https://example.com/watch?v=abc out")]),
        share_request_urlencoded(&[(
            "text",
            "Check this https://example.com/watch?v=abc out",
        )]),
    ] {
        let response = handlers::handle_share(request).await.into_response();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = location_of(response);
        assert!(location.starts_with("/?share="));
        assert!(location.contains("https%3A%2F%2Fexample.com%2Fwatch"));
    }
}

#[tokio::test]
async fn share_without_url_falls_back_to_root() {
    for request in [
        share_request_multipart(&[]),
        share_request_multipart(&[("url", "not a url")]),
        share_request_urlencoded(&[]),
        share_request_urlencoded(&[("text", "just plain words")]),
    ] {
        let response = handlers::handle_share(request).await.into_response();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(location_of(response), "/");
    }
}

// ============================================================================
// Analyze / Download validation (fails before any subprocess starts)
// ============================================================================

fn download_request(url: &str) -> Json<DownloadRequest> {
    Json(DownloadRequest {
        url: url.to_string(),
        format: "video".to_string(),
        quality: "best".to_string(),
        playlist_urls: None,
        lyrics: false,
    })
}

/// Literal public IP keeps validation tests free of DNS lookups.
const PUBLIC_URL: &str = "https://93.184.216.34/watch?v=abc";

fn api_addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41_000)))
}

#[tokio::test]
async fn analyze_rejects_invalid_url_before_subprocess() {
    let state = fresh_state();

    for bad in ["ftp://example.com/v", "", "--config-file=/tmp/evil"] {
        let result = handlers::analyze_video(
            State(state.clone()),
            api_addr(),
            HeaderMap::new(),
            download_request(bad),
        )
        .await;

        match result {
            Err(athena::AppError::InvalidUrl { .. }) => {}
            other => panic!("expected InvalidUrl for {:?}, got {:?}", bad, other.map(|_| ())),
        }
    }

    // Nothing may have been queued.
    assert!(state.active_downloads.lock().await.is_empty());
}

#[tokio::test]
async fn start_download_rejects_invalid_playlist_urls_before_subprocess() {
    let state = fresh_state();

    let request = Json(DownloadRequest {
        url: PUBLIC_URL.to_string(),
        format: "video".to_string(),
        quality: "best".to_string(),
        playlist_urls: Some(vec![
            "https://93.184.216.34/watch?v=ok".to_string(),
            "javascript:alert(1)".to_string(),
        ]),
        lyrics: false,
    });

    let result =
        handlers::start_download(State(state.clone()), api_addr(), HeaderMap::new(), request).await;
    assert!(matches!(result, Err(athena::AppError::InvalidUrl { .. })));
    assert!(state.active_downloads.lock().await.is_empty());
}

// ============================================================================
// Per-IP rate limiting on /api/analyze and /api/download
// ============================================================================

#[tokio::test]
async fn analyze_is_rate_limited_per_ip() {
    let state = fresh_state();

    // Invalid URLs fail before any subprocess, but still consume budget.
    for _ in 0..*MAX_ANALYZE_PER_WINDOW {
        let result = handlers::analyze_video(
            State(state.clone()),
            api_addr(),
            HeaderMap::new(),
            download_request("ftp://example.com/v"),
        )
        .await;
        assert!(
            matches!(result, Err(athena::AppError::InvalidUrl { .. })),
            "requests below the limit must fail with InvalidUrl"
        );
    }

    let result = handlers::analyze_video(
        State(state.clone()),
        api_addr(),
        HeaderMap::new(),
        download_request("ftp://example.com/v"),
    )
    .await;
    match result {
        Err(err @ athena::AppError::TooManyRequests { .. }) => {
            let response = err.into_response();
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            assert!(response.headers().contains_key(header::RETRY_AFTER));
        }
        other => panic!("expected TooManyRequests, got {:?}", other.map(|_| ())),
    }

    // A different client IP still has its own budget.
    let other_ip = ConnectInfo(SocketAddr::from(([192, 0, 2, 7], 50_000)));
    let result =
        handlers::analyze_video(State(state), other_ip, HeaderMap::new(), download_request("ftp://example.com/v"))
            .await;
    assert!(
        matches!(result, Err(athena::AppError::InvalidUrl { .. })),
        "other IPs keep their own budget"
    );
}

#[tokio::test]
async fn downloads_are_rate_limited_per_ip_independently_of_analyze() {
    let state = fresh_state();

    for _ in 0..*MAX_DOWNLOADS_PER_WINDOW {
        let result = handlers::start_download(
            State(state.clone()),
            api_addr(),
            HeaderMap::new(),
            download_request("ftp://example.com/v"),
        )
        .await;
        assert!(matches!(result, Err(athena::AppError::InvalidUrl { .. })));
    }

    let result = handlers::start_download(
        State(state.clone()),
        api_addr(),
        HeaderMap::new(),
        download_request("ftp://example.com/v"),
    )
    .await;
    assert!(
        matches!(result, Err(athena::AppError::TooManyRequests { .. })),
        "download endpoint must have its own budget"
    );

    // Analyze budget is untouched by download traffic.
    let result = handlers::analyze_video(
        State(state),
        api_addr(),
        HeaderMap::new(),
        download_request("ftp://example.com/v"),
    )
    .await;
    assert!(
        matches!(result, Err(athena::AppError::InvalidUrl { .. })),
        "analyze budget must be independent of the download bucket"
    );
}

// ============================================================================
// Progress / file endpoints for unknown downloads
// ============================================================================

const TOKEN: &str = "handler-test-token";

#[tokio::test]
async fn progress_stream_reports_missing_download() {
    let state = fresh_state();
    register_token(&state, TOKEN).await;

    let sse = handlers::progress_stream(
        State(state),
        bearer_headers(TOKEN),
        Path("missing-id".to_string()),
        Query(HashMap::new()),
    )
    .await;

    let body = sse.into_response().into_body();
    let first_chunk = body
        .into_data_stream()
        .next()
        .await
        .expect("at least one SSE event")
        .expect("chunk readable");
    let text = String::from_utf8_lossy(&first_chunk);

    assert!(text.contains("Download not found"), "got: {}", text);
}

#[tokio::test]
async fn download_file_unknown_id_is_not_found() {
    let state = fresh_state();
    register_token(&state, TOKEN).await;

    let result = handlers::download_file(
        State(state.clone()),
        bearer_headers(TOKEN),
        Path("missing-id".to_string()),
        Query(HashMap::new()),
    )
    .await;

    match result {
        Err(athena::AppError::DownloadNotFound { id }) => {
            assert_eq!(id, "missing-id");
            let response = athena::AppError::DownloadNotFound { id }.into_response();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        other => panic!("expected DownloadNotFound, got {:?}", other.map(|_| ())),
    }
}

#[tokio::test]
async fn download_file_not_ready_is_404_for_queued_status() {
    use athena::state::DownloadInfo;

    let state = fresh_state();
    register_token(&state, TOKEN).await;
    {
        let mut downloads = state.active_downloads.lock().await;
        downloads.insert(
            "pending-id".to_string(),
            DownloadInfo {
                status: "processing".to_string(),
                progress: 10.0,
                file_path: None,
                file_name: None,
                error: None,
                timestamp: now_secs(),
                speed: None,
                eta: None,
                last_activity: now_secs(),
                url: None,
                lyrics_plain: None,
                lyrics_synced: None,
            },
        );
    }

    let result = handlers::download_file(
        State(state),
        bearer_headers(TOKEN),
        Path("pending-id".to_string()),
        Query(HashMap::new()),
    )
    .await;

    match result {
        Err(athena::AppError::DownloadNotReady { status }) => {
            assert_eq!(status, "processing");
            let response =
                athena::AppError::DownloadNotReady { status }.into_response();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        other => panic!("expected DownloadNotReady, got {:?}", other.map(|_| ())),

    }
}

async fn read_body(response: Response) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stream = response.into_body().into_data_stream();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk readable"));
    }
    out
}

/// Inserts a completed download entry pointing at a freshly written temp file
/// so that download_file can actually serve bytes.
async fn seed_completed_download(state: &SharedState, id: &str, payload: &[u8]) -> std::path::PathBuf {
    use athena::state::DownloadInfo;

    // Parallel tests may finish within the same second -> sequence number
    // keeps the temp directories disjoint.
    static DIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let dir = std::env::temp_dir().join(format!(
        "athena-test-{}-{}",
        std::process::id(),
        seq
    ));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let file_path = dir.join(format!("{id}.bin"));
    tokio::fs::write(&file_path, payload).await.unwrap();

    let mut downloads = state.active_downloads.lock().await;
    downloads.insert(
        id.to_string(),
        DownloadInfo {
            status: "completed".to_string(),
            progress: 100.0,
            file_path: Some(file_path.clone()),
            file_name: Some(format!("Song-[{id}].mp4")),
            error: None,
            timestamp: now_secs(),
            speed: None,
            eta: None,
            last_activity: now_secs(),
            url: None,
            lyrics_plain: None,
            lyrics_synced: None,
        },
    );
    file_path
}

#[tokio::test]
async fn download_file_serves_full_file_with_range_support_headers() {
    let state = fresh_state();
    register_token(&state, TOKEN).await;
    let payload = b"0123456789abcdef";
    let file_path = seed_completed_download(&state, "full-id", payload).await;

    let response = handlers::download_file(
        State(state),
        bearer_headers(TOKEN),
        Path("full-id".to_string()),
        Query(HashMap::new()),
    )
    .await
    .expect("serving completed download must work")
    .into_response();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "16");
    assert_eq!(read_body(response).await, payload.to_vec());

    tokio::fs::remove_dir_all(file_path.parent().unwrap()).await.unwrap();
}

#[tokio::test]
async fn download_file_answers_range_requests_with_partial_content() {
    let state = fresh_state();
    register_token(&state, TOKEN).await;
    let payload = b"0123456789abcdef";
    let file_path = seed_completed_download(&state, "range-id", payload).await;

    // Simple range: bytes=4-9 -> 206 with exactly those bytes
    let mut headers = bearer_headers(TOKEN);
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=4-9"));
    let response = handlers::download_file(
        State(state.clone()),
        headers,
        Path("range-id".to_string()),
        Query(HashMap::new()),
    )
    .await
    .unwrap()
    .into_response();

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 4-9/16");
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "6");
    assert_eq!(read_body(response).await, b"456789".to_vec());

    // Open-ended range: bytes=12- -> tail of the file
    let mut headers = bearer_headers(TOKEN);
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=12-"));
    let response = handlers::download_file(
        State(state.clone()),
        headers,
        Path("range-id".to_string()),
        Query(HashMap::new()),
    )
    .await
    .unwrap()
    .into_response();

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 12-15/16");
    assert_eq!(read_body(response).await, b"cdef".to_vec());

    // Suffix range: bytes=-4 -> last four bytes
    let mut headers = bearer_headers(TOKEN);
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=-4"));
    let response = handlers::download_file(
        State(state.clone()),
        headers,
        Path("range-id".to_string()),
        Query(HashMap::new()),
    )
    .await
    .unwrap()
    .into_response();

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 12-15/16");
    assert_eq!(read_body(response).await, b"cdef".to_vec());

    tokio::fs::remove_dir_all(file_path.parent().unwrap()).await.unwrap();
}

#[tokio::test]
async fn download_file_rejects_unsatisfiable_range_with_416() {
    let state = fresh_state();
    register_token(&state, TOKEN).await;
    let payload = b"0123456789abcdef";
    let file_path = seed_completed_download(&state, "r416-id", payload).await;

    let mut headers = bearer_headers(TOKEN);
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=999-2000"));
    let response = handlers::download_file(
        State(state),
        headers,
        Path("r416-id".to_string()),
        Query(HashMap::new()),
    )
    .await
    .unwrap()
    .into_response();

    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */16");

    tokio::fs::remove_dir_all(file_path.parent().unwrap()).await.unwrap();
}

// ============================================================================
// Authentication
// ============================================================================

#[tokio::test]
async fn is_authenticated_with_registered_token() {
    let state = fresh_state();
    register_token(&state, TOKEN).await;

    // A registered token is accepted regardless of whether auth is enabled.
    assert!(
        handlers::is_authenticated(&bearer_headers(TOKEN), None, &state).await,
        "registered token must authenticate"
    );
}

#[tokio::test]
async fn is_authenticated_expiry_boundary_respected() {
    let state = fresh_state();
    let now = now_secs();
    {
        let mut tokens = state.auth_tokens.lock().await;
        tokens.insert("expired-token".to_string(), now - TOKEN_MAX_AGE_SECS - 1.0);
        tokens.insert("fresh-token".to_string(), now);
    }

    if auth_enabled() {
        assert!(!handlers::is_authenticated(
            &bearer_headers("expired-token"),
            None,
            &state
        )
        .await);
        assert!(handlers::is_authenticated(
            &bearer_headers("fresh-token"),
            None,
            &state
        )
        .await);
    } else {
        // Without auth enabled everything is allowed by design.
        assert!(handlers::is_authenticated(
            &bearer_headers("expired-token"),
            None,
            &state
        )
        .await);
    }
}

fn login_addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 45_820)))
}

#[tokio::test]
async fn login_password_flow_matches_auth_mode() {
    let state = fresh_state();

    if auth_enabled() {
        // Wrong password must be rejected.
        let wrong = handlers::login(
            State(state.clone()),
            login_addr(),
            Json(LoginRequest {
                password: "definitely-wrong-password".to_string(),
            }),
        )
        .await;
        match wrong {
            Err(athena::AppError::Unauthorized { message }) => {
                assert!(message.contains("Invalid password"));
            }
            Ok(_) => panic!("wrong password must be rejected"),
            other => panic!("unexpected error variant: {:?}", other.err()),
        }

        // Correct password (whatever ATHENA_PASSWORD is) must issue a token.
        let password = std::env::var("ATHENA_PASSWORD").unwrap_or_default();
        let correct = handlers::login(
            State(state.clone()),
            login_addr(),
            Json(LoginRequest { password }),
        )
        .await;
        match correct {
            Ok(Json(ApiResponse { success, data, .. })) => {
                assert!(success);
                assert!(!data.expect("token present").token.is_empty());
            }
            Err(e) => panic!("valid login must succeed, got: {}", e),
        }
    } else {
        let response = handlers::login(
            State(state.clone()),
            login_addr(),
            Json(LoginRequest {
                password: "anything".to_string(),
            }),
        )
        .await
        .expect("login succeeds without auth");

        let Json(ApiResponse { success, data, .. }) = response;
        assert!(success);
        assert_eq!(data.expect("token").token, "");
    }
}

#[tokio::test]
async fn login_rate_limiting_blocks_after_max_attempts() {
    let state = fresh_state();
    let addr = login_addr();

    for attempt in 0..6usize {
        let result = handlers::login(
            State(state.clone()),
            addr,
            Json(LoginRequest {
                password: format!("guess-{}", attempt),
            }),
        )
        .await;

        if auth_enabled() && attempt >= 5 {
            match result {
                Err(err) => {
                    assert!(
                        err.to_string().contains("Zu viele Fehlversuche"),
                        "rate limit message expected, got: {}",
                        err
                    );
                }
                Ok(_) => panic!("6th attempt must be rate limited"),
            }
        }
    }

    let attempts = state.login_attempts.lock().await;
    if auth_enabled() {
        assert!(!attempts.is_empty(), "failed attempts must be recorded");
    }
}

// ============================================================================
// Logout / token invalidation
// ============================================================================

#[tokio::test]
async fn logout_invalidates_the_presented_token() {
    let state = fresh_state();
    register_token(&state, TOKEN).await;

    let Json(ApiResponse { success, data, .. }) =
        handlers::logout(State(state.clone()), bearer_headers(TOKEN), Query(HashMap::new())).await;

    assert!(success);
    assert_eq!(data.expect("payload")["logged_out"], serde_json::json!(true));
    assert!(
        state.auth_tokens.lock().await.is_empty(),
        "token must be removed from the server-side store"
    );
}

#[tokio::test]
async fn logout_is_idempotent_for_unknown_or_missing_tokens() {
    let state = fresh_state();

    // No token at all
    let Json(ApiResponse { data, .. }) =
        handlers::logout(State(state.clone()), HeaderMap::new(), Query(HashMap::new())).await;
    assert_eq!(data.expect("payload")["logged_out"], serde_json::json!(false));

    // Unknown bearer token
    let Json(ApiResponse { data, .. }) = handlers::logout(
        State(state.clone()),
        bearer_headers("never-issued"),
        Query(HashMap::new()),
    )
    .await;
    assert_eq!(data.expect("payload")["logged_out"], serde_json::json!(false));

    // A second logout with an already-invalidated token stays harmless
    register_token(&state, TOKEN).await;
    let _ = handlers::logout(State(state.clone()), bearer_headers(TOKEN), Query(HashMap::new())).await;
    let Json(ApiResponse { data, .. }) =
        handlers::logout(State(state), bearer_headers(TOKEN), Query(HashMap::new())).await;
    assert_eq!(data.expect("payload")["logged_out"], serde_json::json!(false));
}
