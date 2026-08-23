use axum::{
    body::Body,
    extract::Request,
    http::{header::CACHE_CONTROL, HeaderValue},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tower_http::{compression::CompressionLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::prelude::*;

pub mod errors;
pub mod handlers;
pub mod metadata;
pub mod models;
pub mod state;
pub mod tags;
pub mod ytdlp;

use crate::state::{AppState, SharedState, DOWNLOAD_DIR, DOWNLOAD_SEMAPHORE, PASSWORD_HASH};
use crate::ytdlp::{periodic_yt_dlp_update, update_yt_dlp};

const MAX_BODY_BYTES: usize = 256 * 1024;

/// Defense-in-depth against XSS through externally sourced metadata
/// (video titles, playlist names, ...).
///
/// The frontend ships as a single inlined HTML file (vite-plugin-singlefile),
/// so scripts and styles arrive as inline tags and need 'unsafe-inline'.
/// Thumbnails are hot-linked from external CDNs (img-src https:), the favicon
/// is a data: URI. Everything else is locked to the app origin.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
    script-src 'self' 'unsafe-inline'; \
    style-src 'self' 'unsafe-inline'; \
    img-src 'self' https: data:; \
    media-src 'self' https:; \
    connect-src 'self'; \
    font-src 'self'; \
    object-src 'none'; \
    base-uri 'self'; \
    form-action 'self'; \
    frame-ancestors 'none'";

fn bind_error_hint(port: u16, err: &std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::AddrInUse {
        format!(
            "Fehler: Port {} ist bereits belegt.\n\
             Höchstwahrscheinlich läuft der Athena-Systemd-Service bereits.\n\n\
             Status prüfen : sudo systemctl status athena\n\
             Neu starten   : sudo systemctl restart athena\n\
             Stoppen       : sudo systemctl stop athena\n\n\
             Oder parallel mit anderem Port starten:\n  PORT={} athena",
            port,
            port + 1
        )
    } else {
        format!(
            "Fehler: Server konnte nicht auf Port {} starten: {}\n\
             Alternativ einen anderen Port nutzen: PORT=8080 athena",
            port, err
        )
    }
}

async fn security_headers(req: Request<Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    if !headers.contains_key(CACHE_CONTROL) {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "referrer-policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    res
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    // 1. Setup daily rotating structured logging to file + stdout
    let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| ".".to_string());
    let file_appender = tracing_appender::rolling::daily(log_dir, "athena.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false);

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(true);

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(stdout_layer)
        .with(file_layer)
        .init();

    // Ensure download directory exists
    tokio::fs::create_dir_all(&*DOWNLOAD_DIR)
        .await
        .expect("Failed to create download directory");

    info!("Download directory: {:?}", *DOWNLOAD_DIR);
    info!(
        "Max concurrent downloads: {}",
        DOWNLOAD_SEMAPHORE.available_permits()
    );

    // Check yt-dlp version and update if needed
    tokio::spawn(async {
        update_yt_dlp().await;
    });

    // Initialize state
    let state: SharedState = Arc::new(AppState {
        active_downloads: tokio::sync::Mutex::new(HashMap::new()),
        auth_tokens: tokio::sync::Mutex::new(HashMap::new()),
        active_children: tokio::sync::Mutex::new(HashMap::new()),
        login_attempts: tokio::sync::Mutex::new(HashMap::new()),
        api_rate_limits: tokio::sync::Mutex::new(HashMap::new()),
        metadata_cache: tokio::sync::Mutex::new(HashMap::new()),
    });

    if PASSWORD_HASH.is_some() {
        info!("Password protection enabled");
    } else {
        info!("No password set - running without authentication");
    }

    // Start cleanup task
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        state::periodic_cleanup(cleanup_state).await;
    });

    // Start yt-dlp auto-update task (checks every 24 hours)
    tokio::spawn(async {
        periodic_yt_dlp_update().await;
    });

    // Static assets served with gzip compression (HTML shrinks from ~40 KB to ~10 KB)
    let static_routes = Router::new()
        .route("/", get(handlers::root))
        .route("/manifest.json", get(handlers::manifest_handler))
        .route("/sw.js", get(handlers::sw_handler))
        .route("/app-icon.png", get(handlers::icon_handler))
        .route("/favicon.ico", get(handlers::icon_handler))
        .layer(CompressionLayer::new());

    // Build router
    let app = Router::new()
        .merge(static_routes)
        .route("/share", post(handlers::handle_share))
        .route("/api/config", get(handlers::get_config))
        .route("/api/login", post(handlers::login))
        .route("/api/logout", post(handlers::logout))
        .route("/api/analyze", post(handlers::analyze_video))
        .route("/api/download", post(handlers::start_download))
        .route("/api/abort/:download_id", post(handlers::abort_download))
        .route(
            "/api/heartbeat/:download_id",
            post(handlers::heartbeat_download),
        )
        .route(
            "/api/release/:download_id",
            post(handlers::release_download),
        )
        .route("/api/progress/:download_id", get(handlers::progress_stream))
        .route("/api/file/:download_id", get(handlers::download_file))
        .route("/api/ytdlp-update", post(handlers::trigger_ytdlp_update))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(middleware::from_fn(security_headers))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let port = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8000);

    let bind_addr = format!("0.0.0.0:{}", port);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{}", bind_error_hint(port, &e));
            std::process::exit(1);
        }
    };

    info!("Athena server listening on 0.0.0.0:{}", port);

    // Serve app with graceful shutdown signal and Connection Info extraction
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(state))
    .await
    .unwrap();
}

async fn shutdown_signal(state: SharedState) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received. Starting graceful shutdown...");

    // 1. Kill active download subprocesses
    {
        let mut children = state.active_children.lock().await;
        for (id, mut child) in children.drain() {
            info!("Killing active download process: {}", id);
            let _ = child.kill().await;
        }
    }

    // 2. Clean up temporary files in DOWNLOAD_DIR
    info!(
        "Cleaning up temporary download directory: {:?}",
        *DOWNLOAD_DIR
    );
    if let Ok(mut entries) = tokio::fs::read_dir(&*DOWNLOAD_DIR).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if ext == "part" || ext == "ytdl" {
                        info!("Removing incomplete temp file: {:?}", path);
                        let _ = tokio::fs::remove_file(&path).await;
                    }
                }
            }
        }
    }

    info!("Graceful shutdown complete. Exiting.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bind_error_hint_addr_in_use() {
        let err = std::io::Error::new(std::io::ErrorKind::AddrInUse, "Address already in use");
        let hint = bind_error_hint(8000, &err);

        assert!(hint.contains("8000"));
        assert!(hint.contains("bereits belegt"));
        assert!(hint.contains("systemctl status athena"));
        assert!(hint.contains("systemctl restart athena"));
        assert!(hint.contains("PORT=8001"));
    }

    #[test]
    fn test_bind_error_hint_addr_in_use_alternative_port() {
        let err = std::io::Error::new(std::io::ErrorKind::AddrInUse, "busy");
        let hint = bind_error_hint(9000, &err);
        assert!(hint.contains("PORT=9001"));
    }

    #[test]
    fn test_bind_error_hint_other_error() {
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "no permission");
        let hint = bind_error_hint(80, &err);

        assert!(hint.contains("80"));
        assert!(hint.contains("no permission"));
        assert!(!hint.contains("bereits belegt"));
        assert!(hint.contains("PORT=8080"));
    }

    #[test]
    fn test_max_body_bytes_sane() {
        const _: () = const {
            assert!(MAX_BODY_BYTES >= 64 * 1024, "body limit too small");
            assert!(MAX_BODY_BYTES < 1024 * 1024, "body limit too large");
        };
    }
}
