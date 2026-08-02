use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
};
use axum::{
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::prelude::*;

pub mod errors;
pub mod models;
pub mod state;
pub mod ytdlp;
pub mod handlers;

use crate::state::{AppState, SharedState, DOWNLOAD_DIR, DOWNLOAD_SEMAPHORE, PASSWORD_HASH};
use crate::ytdlp::{periodic_yt_dlp_update, update_yt_dlp};

#[tokio::main]
async fn main() {
    // Load environment variables from .env file
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
    info!("Max concurrent downloads: {}", DOWNLOAD_SEMAPHORE.available_permits());

    // Check yt-dlp version and update if needed
    tokio::spawn(async {
        update_yt_dlp().await;
    });

    // Initialize state
    let state = Arc::new(AppState {
        active_downloads: tokio::sync::Mutex::new(HashMap::new()),
        auth_tokens: tokio::sync::Mutex::new(HashMap::new()),
        active_children: tokio::sync::Mutex::new(HashMap::new()),
        login_attempts: tokio::sync::Mutex::new(HashMap::new()),
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

    // Build router
    let app = Router::new()
        .route("/", get(handlers::root))
        .route("/manifest.json", get(handlers::manifest_handler))
        .route("/sw.js", get(handlers::sw_handler))
        .route("/app-icon.png", get(handlers::icon_handler))
        .route("/share", post(handlers::handle_share))
        .route("/api/config", get(handlers::get_config))
        .route("/api/login", post(handlers::login))
        .route("/api/analyze", post(handlers::analyze_video))
        .route("/api/download", post(handlers::start_download))
        .route("/api/progress/:download_id", get(handlers::progress_stream))
        .route("/api/file/:download_id", get(handlers::download_file))
        .route("/api/ytdlp-update", post(handlers::trigger_ytdlp_update))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let port = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8000);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap();

    info!("Athena server listening on 0.0.0.0:{}", port);

    // Serve app with graceful shutdown signal and Connection Info extraction
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>()
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
    info!("Cleaning up temporary download directory: {:?}", *DOWNLOAD_DIR);
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
