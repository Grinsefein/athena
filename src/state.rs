use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    sync::{Mutex, Semaphore},
    time::interval,
};
use tracing::{info, warn};

// Password protection
pub static PASSWORD_HASH: Lazy<Option<String>> = Lazy::new(|| {
    std::env::var("ATHENA_PASSWORD").ok().map(|p| {
        let mut hasher = Sha256::new();
        hasher.update(p.as_bytes());
        hex::encode(hasher.finalize())
    })
});

// Configuration - use system temp directory for temporary storage
pub static DOWNLOAD_DIR: Lazy<PathBuf> = Lazy::new(|| {
    std::env::var("DOWNLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("athena-downloads"))
});

// Short max age for temp files (1 hour)
pub static MAX_FILE_AGE_HOURS: Lazy<f64> = Lazy::new(|| {
    std::env::var("MAX_FILE_AGE_HOURS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0)
});

pub static CLEANUP_INTERVAL_SECONDS: Lazy<u64> = Lazy::new(|| {
    std::env::var("CLEANUP_INTERVAL_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600)
});

// Global semaphore for concurrent downloads
pub static DOWNLOAD_SEMAPHORE: Lazy<Arc<Semaphore>> = Lazy::new(|| {
    let max = std::env::var("MAX_CONCURRENT_DOWNLOADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    Arc::new(Semaphore::new(max))
});

pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub active_downloads: Mutex<HashMap<String, DownloadInfo>>,
    pub auth_tokens: Mutex<HashMap<String, ()>>, // Simple token store
    pub active_children: Mutex<HashMap<String, tokio::process::Child>>, // Active yt-dlp child processes
    pub login_attempts: Mutex<HashMap<String, Vec<f64>>>, // Login rate-limiting timestamps by IP
}

#[derive(Clone, Debug)]
pub struct DownloadInfo {
    pub status: String,
    pub progress: f64,
    pub file_path: Option<PathBuf>,
    pub file_name: Option<String>,
    pub error: Option<String>,
    pub timestamp: f64,
    pub speed: Option<String>,
    pub eta: Option<String>,
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub async fn periodic_cleanup(state: SharedState) {
    let mut interval = interval(Duration::from_secs(*CLEANUP_INTERVAL_SECONDS));

    loop {
        interval.tick().await;
        cleanup_old_data(&state).await;
    }
}

pub async fn cleanup_old_data(state: &AppState) {
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

                                if current_hours - age_hours > *MAX_FILE_AGE_HOURS {
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
                age_hours > *MAX_FILE_AGE_HOURS
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in stale_ids {
            downloads.remove(&id);
            info!("Cleaned up stale memory entry: {}", id);
        }
    }
}

// ============================================================================
// Unit Tests
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_download_dir_lazy() {
        let dir = &*DOWNLOAD_DIR;
        assert!(dir.is_absolute() || dir.starts_with("."));
    }

    #[test]
    fn test_download_semaphore() {
        let permits = DOWNLOAD_SEMAPHORE.available_permits();
        assert!(permits > 0);
    }

    #[tokio::test]
    async fn test_app_state_creation() {
        let state = Arc::new(AppState {
            active_downloads: Mutex::new(HashMap::new()),
            auth_tokens: Mutex::new(HashMap::new()),
            active_children: Mutex::new(HashMap::new()),
            login_attempts: Mutex::new(HashMap::new()),
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
                speed: None,
                eta: None,
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
            auth_tokens: Mutex::new(HashMap::new()),
            active_children: Mutex::new(HashMap::new()),
            login_attempts: Mutex::new(HashMap::new()),
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
                speed: None,
                eta: None,
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
        assert_eq!(*MAX_FILE_AGE_HOURS, 1.0);
        assert_eq!(*CLEANUP_INTERVAL_SECONDS, 600);
    }
}
