use once_cell::sync::Lazy;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs,
    sync::{Mutex, Semaphore},
    time::interval,
};
use tracing::{info, warn};

// Password protection
//
// ATHENA_PASSWORD accepts either format:
// - a bcrypt hash ("$2a$", "$2b$" or "$2y$" prefix) -> recommended, generate
//   one with `cargo run --bin hash-password -- 'your password'`
// - plaintext (legacy behaviour, verified in constant time) -> still works,
//   but a deprecation warning is logged on first use
pub static PASSWORD_HASH: Lazy<Option<String>> = Lazy::new(|| {
    std::env::var("ATHENA_PASSWORD")
        .ok()
        .filter(|p| !p.trim().is_empty())
});

fn is_bcrypt_hash(value: &str) -> bool {
    (value.starts_with("$2a$") || value.starts_with("$2b$") || value.starts_with("$2y$"))
        && value.len() >= 59
}

/// Constant-time comparison of two strings of equal length.
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// One-time hint pointing at the migration path for plaintext credentials.
static LEGACY_PASSWORD_WARNING: Lazy<()> = Lazy::new(|| {
    warn!(
        "ATHENA_PASSWORD ist kein bcrypt-Hash. Empfohlen: cargo run --bin hash-password -- '<Passwort>' und den Hash in .env eintragen."
    );
});

/// Check a login candidate against the configured credential.
///
/// bcrypt values are verified with the salt baked into the hash; anything
/// else falls back to the exact constant-time plaintext comparison that
/// releases <= 1.5 performed (there the SHA-256 was computed identically on
/// both sides, so this is behaviour-preserving while dropping the pointless
/// double hashing).
pub fn verify_password_against(stored: &str, candidate: &str) -> bool {
    if is_bcrypt_hash(stored) {
        return bcrypt::verify(candidate, stored).unwrap_or(false);
    }
    Lazy::force(&LEGACY_PASSWORD_WARNING);
    constant_time_eq(stored, candidate)
}

/// Verify a login candidate against the configured ATHENA_PASSWORD value.
pub fn verify_password(candidate: &str) -> bool {
    PASSWORD_HASH
        .as_deref()
        .map(|stored| verify_password_against(stored, candidate))
        .unwrap_or(false)
}

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

// Global semaphore for concurrent analyze calls (each spawns real yt-dlp processes)
pub static ANALYZE_SEMAPHORE: Lazy<Arc<Semaphore>> = Lazy::new(|| {
    let max = std::env::var("MAX_CONCURRENT_ANALYZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    Arc::new(Semaphore::new(max))
});

// TTL for cached yt-dlp metadata (in hours)
pub static META_CACHE_TTL_HOURS: Lazy<f64> = Lazy::new(|| {
    std::env::var("META_CACHE_TTL_HOURS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6.0)
});

// How long a completed download is kept on disk after the last sign of life
// (heartbeat, serve or progress event) from its session.
pub static FILE_RETENTION_MINUTES: Lazy<f64> = Lazy::new(|| {
    std::env::var("FILE_RETENTION_MINUTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0)
});

pub const META_CACHE_MAX_ENTRIES: usize = 128;

pub const TOKEN_MAX_AGE_SECS: f64 = 30.0 * 24.0 * 3600.0;
pub const LOGIN_WINDOW_SECS: f64 = 900.0;
pub const MAX_LOGIN_ATTEMPTS: usize = 5;

// Per-IP request rate limiting for the expensive API endpoints
// (/api/analyze, /api/download). Sliding window of timestamps per key.
pub const API_WINDOW_SECS: f64 = 60.0;

pub static MAX_ANALYZE_PER_WINDOW: Lazy<usize> = Lazy::new(|| {
    std::env::var("MAX_ANALYZE_PER_MINUTE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12)
});

pub static MAX_DOWNLOADS_PER_WINDOW: Lazy<usize> = Lazy::new(|| {
    std::env::var("MAX_DOWNLOADS_PER_MINUTE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6)
});

pub type SharedState = Arc<AppState>;

#[derive(Clone, Debug)]
pub struct CachedMeta {
    pub json: serde_json::Value,
    pub fetched_at: f64,
}

pub struct AppState {
    pub active_downloads: Mutex<HashMap<String, DownloadInfo>>,
    pub auth_tokens: Mutex<HashMap<String, f64>>, // Token -> creation timestamp
    pub active_children: Mutex<HashMap<String, tokio::process::Child>>, // Active yt-dlp child processes
    pub login_attempts: Mutex<HashMap<String, Vec<f64>>>, // Login rate-limiting timestamps by IP
    pub api_rate_limits: Mutex<HashMap<String, Vec<f64>>>, // Sliding-window request timestamps keyed by "{endpoint}:{ip}"
    pub metadata_cache: Mutex<HashMap<String, CachedMeta>>, // canonical URL -> yt-dlp info JSON
}

/// Sliding-window rate limiter over a shared timestamp bucket map.
///
/// Records a request for `key` and returns `false` when the key already
/// accumulated `max` requests inside `window_secs` (the rejected request is
/// NOT recorded, so clients retrying into the limit cannot extend their ban).
pub async fn consume_rate_limit(
    buckets: &Mutex<HashMap<String, Vec<f64>>>,
    key: &str,
    max: usize,
    window_secs: f64,
) -> bool {
    let now = now_secs();
    let mut buckets = buckets.lock().await;
    let timestamps = buckets.entry(key.to_string()).or_default();
    timestamps.retain(|&t| now - t < window_secs);
    if timestamps.len() >= max {
        return false;
    }
    timestamps.push(now);
    true
}

fn evict_metadata_cache(cache: &mut HashMap<String, CachedMeta>) -> usize {
    let now = now_secs();
    let ttl_secs = *META_CACHE_TTL_HOURS * 3600.0;
    cache.retain(|_, meta| now - meta.fetched_at < ttl_secs);

    let mut removed = 0;
    if cache.len() >= META_CACHE_MAX_ENTRIES {
        let oldest = cache
            .iter()
            .min_by(|a, b| {
                a.1.fetched_at
                    .partial_cmp(&b.1.fetched_at)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(k, _)| k.clone());
        if let Some(key) = oldest {
            cache.remove(&key);
            removed += 1;
        }
    }
    removed
}

pub async fn get_cached_meta(state: &AppState, key: &str) -> Option<serde_json::Value> {
    let ttl_secs = *META_CACHE_TTL_HOURS * 3600.0;
    let cache = state.metadata_cache.lock().await;
    let meta = cache.get(key)?;
    if now_secs() - meta.fetched_at < ttl_secs {
        Some(meta.json.clone())
    } else {
        None
    }
}

pub async fn store_cached_meta(state: &AppState, key: String, json: serde_json::Value) {
    let mut cache = state.metadata_cache.lock().await;
    evict_metadata_cache(&mut cache);
    cache.insert(
        key,
        CachedMeta {
            json,
            fetched_at: now_secs(),
        },
    );
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
    pub last_activity: f64,
    pub url: Option<String>,
    pub lyrics_plain: Option<String>,
    pub lyrics_synced: Option<String>,
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Remove a download entry together with its file on disk.
/// Returns true when an entry existed and was removed.
pub async fn delete_download_artifacts(state: &AppState, download_id: &str) -> bool {
    let info = {
        let mut downloads = state.active_downloads.lock().await;
        downloads.remove(download_id)
    };

    match info {
        Some(info) => {
            if let Some(path) = info.file_path {
                if let Err(e) = fs::remove_file(&path).await {
                    warn!("Failed to remove file {:?} of {}: {}", path, download_id, e);
                }
            }
            true
        }
        None => false,
    }
}

/// Refresh the session liveness timestamp of a download, if it still exists.
pub async fn touch_download(state: &AppState, download_id: &str) -> bool {
    let mut downloads = state.active_downloads.lock().await;
    match downloads.get_mut(download_id) {
        Some(info) => {
            info.last_activity = now_secs();
            true
        }
        None => false,
    }
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

    // Session-driven deletion: completed downloads whose session showed no
    // sign of life (heartbeat/serve/progress) for longer than the retention
    // window lose their file and entry.
    let expired_ids: Vec<String> = {
        let downloads = state.active_downloads.lock().await;
        let ttl_secs = *FILE_RETENTION_MINUTES * 60.0;
        downloads
            .iter()
            .filter(|(_, info)| {
                info.status == "completed" && current_time - info.last_activity > ttl_secs
            })
            .map(|(id, _)| id.clone())
            .collect()
    };

    for id in &expired_ids {
        delete_download_artifacts(state, id).await;
        info!("Retention expired, removed download {}", id);
    }

    // Snapshot file paths that still belong to live entries; the orphan sweep
    // below must never touch them.
    let referenced_paths: std::collections::HashSet<PathBuf> = {
        let downloads = state.active_downloads.lock().await;
        downloads
            .values()
            .filter_map(|i| i.file_path.clone())
            .collect()
    };

    // Cleanup old orphaned files (no owning entry anymore)
    match fs::read_dir(&*DOWNLOAD_DIR).await {
        Ok(mut entries) => {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_file() && !referenced_paths.contains(&path) {
                    match entry.metadata().await {
                        Ok(metadata) => {
                            if let Ok(modified) = metadata.modified() {
                                let age_hours = modified
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                                    as f64
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

    // Cleanup stale memory entries in single lock
    {
        let mut downloads = state.active_downloads.lock().await;
        let stale_ids: Vec<String> = downloads
            .iter()
            .filter(|(_, info)| (current_time - info.timestamp) / 3600.0 > *MAX_FILE_AGE_HOURS)
            .map(|(id, _)| id.clone())
            .collect();

        for id in stale_ids {
            downloads.remove(&id);
            info!("Cleaned up stale memory entry: {}", id);
        }
    }

    // Cleanup expired auth tokens
    {
        let mut tokens = state.auth_tokens.lock().await;
        let before = tokens.len();
        tokens.retain(|_, created| current_time - *created < TOKEN_MAX_AGE_SECS);
        if before != tokens.len() {
            info!("Cleaned up {} expired auth tokens", before - tokens.len());
        }
    }

    // Cleanup stale login rate-limit entries
    {
        let mut attempts = state.login_attempts.lock().await;
        for timestamps in attempts.values_mut() {
            timestamps.retain(|&t| current_time - t < LOGIN_WINDOW_SECS);
        }
        attempts.retain(|_, timestamps| !timestamps.is_empty());
    }

    // Cleanup stale API request-rate entries
    {
        let mut buckets = state.api_rate_limits.lock().await;
        for timestamps in buckets.values_mut() {
            timestamps.retain(|&t| current_time - t < API_WINDOW_SECS);
        }
        buckets.retain(|_, timestamps| !timestamps.is_empty());
    }

    // Cleanup expired metadata cache entries
    {
        let mut cache = state.metadata_cache.lock().await;
        let before = cache.len();
        evict_metadata_cache(&mut cache);
        if before != cache.len() {
            info!(
                "Cleaned up {} expired metadata cache entries",
                before - cache.len()
            );
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
            api_rate_limits: Mutex::new(HashMap::new()),
            metadata_cache: Mutex::new(HashMap::new()),
        });

        // Test inserting a download
        {
            let mut downloads = state.active_downloads.lock().await;
            downloads.insert(
                "test-id".to_string(),
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
                    url: None,
                    lyrics_plain: None,
                    lyrics_synced: None,
                },
            );
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
            api_rate_limits: Mutex::new(HashMap::new()),
            metadata_cache: Mutex::new(HashMap::new()),
        });

        // Insert
        {
            let mut downloads = state.active_downloads.lock().await;
            downloads.insert(
                "test-id".to_string(),
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
                    url: None,
                    lyrics_plain: None,
                    lyrics_synced: None,
                },
            );
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

#[cfg(test)]
mod auth_cleanup_tests {
    use super::*;

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

    #[tokio::test]
    async fn test_expired_tokens_removed() {
        let state = fresh_state();
        let now = now_secs();
        {
            let mut tokens = state.auth_tokens.lock().await;
            tokens.insert("expired".to_string(), now - TOKEN_MAX_AGE_SECS - 10.0);
            tokens.insert("fresh".to_string(), now);
        }

        cleanup_old_data(&state).await;

        let tokens = state.auth_tokens.lock().await;
        assert!(!tokens.contains_key("expired"));
        assert!(tokens.contains_key("fresh"));
    }

    #[tokio::test]
    async fn test_token_exactly_at_max_age_is_removed() {
        let state = fresh_state();
        let now = now_secs();
        {
            let mut tokens = state.auth_tokens.lock().await;
            tokens.insert("boundary".to_string(), now - TOKEN_MAX_AGE_SECS);
        }

        cleanup_old_data(&state).await;

        let tokens = state.auth_tokens.lock().await;
        assert!(
            !tokens.contains_key("boundary"),
            "token at exact max age should not be considered valid anymore"
        );
    }

    #[tokio::test]
    async fn test_stale_login_attempts_removed() {
        let state = fresh_state();
        let now = now_secs();
        {
            let mut attempts = state.login_attempts.lock().await;
            attempts.insert(
                "stale-ip".to_string(),
                vec![now - LOGIN_WINDOW_SECS - 1.0, now - LOGIN_WINDOW_SECS - 2.0],
            );
            attempts.insert("active-ip".to_string(), vec![now - 60.0, now - 10.0]);
        }

        cleanup_old_data(&state).await;

        let attempts = state.login_attempts.lock().await;
        assert!(!attempts.contains_key("stale-ip"));
        assert!(attempts.contains_key("active-ip"));
        assert_eq!(attempts.get("active-ip").unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_mixed_login_attempts_partial_retention() {
        let state = fresh_state();
        let now = now_secs();
        {
            let mut attempts = state.login_attempts.lock().await;
            attempts.insert(
                "mixed-ip".to_string(),
                vec![now - LOGIN_WINDOW_SECS - 5.0, now - 30.0],
            );
        }

        cleanup_old_data(&state).await;

        let attempts = state.login_attempts.lock().await;
        assert!(
            attempts.contains_key("mixed-ip"),
            "IP with at least one recent attempt must be kept"
        );
        assert_eq!(attempts.get("mixed-ip").unwrap().len(), 1);
    }

    #[test]
    fn test_auth_constants_sane() {
        assert_eq!(TOKEN_MAX_AGE_SECS, 30.0 * 24.0 * 3600.0);
        assert_eq!(LOGIN_WINDOW_SECS, 900.0);
        assert_eq!(MAX_LOGIN_ATTEMPTS, 5);
    }

    #[tokio::test]
    async fn test_retention_expires_idle_completed_downloads() {
        let state = fresh_state();
        let now = now_secs();
        {
            let mut downloads = state.active_downloads.lock().await;
            downloads.insert(
                "expired".to_string(),
                DownloadInfo {
                    status: "completed".to_string(),
                    progress: 100.0,
                    file_path: None,
                    file_name: None,
                    error: None,
                    timestamp: now,
                    speed: None,
                    eta: None,
                    last_activity: now - *FILE_RETENTION_MINUTES * 60.0 - 5.0,
                    url: None,
                    lyrics_plain: None,
                    lyrics_synced: None,
                },
            );
            downloads.insert(
                "fresh".to_string(),
                DownloadInfo {
                    status: "completed".to_string(),
                    progress: 100.0,
                    file_path: None,
                    file_name: None,
                    error: None,
                    timestamp: now,
                    speed: None,
                    eta: None,
                    last_activity: now,
                    url: None,
                    lyrics_plain: None,
                    lyrics_synced: None,
                },
            );
        }

        cleanup_old_data(&state).await;

        let downloads = state.active_downloads.lock().await;
        assert!(
            !downloads.contains_key("expired"),
            "idle completed download must expire"
        );
        assert!(
            downloads.contains_key("fresh"),
            "recently active download must survive"
        );
    }

    #[tokio::test]
    async fn test_retention_never_touches_running_downloads() {
        let state = fresh_state();
        let now = now_secs();
        {
            let mut downloads = state.active_downloads.lock().await;
            downloads.insert(
                "running".to_string(),
                DownloadInfo {
                    status: "processing".to_string(),
                    progress: 40.0,
                    file_path: None,
                    file_name: None,
                    error: None,
                    timestamp: now,
                    speed: None,
                    eta: None,
                    last_activity: now - *FILE_RETENTION_MINUTES * 60.0 - 60.0,
                    url: None,
                    lyrics_plain: None,
                    lyrics_synced: None,
                },
            );
        }

        cleanup_old_data(&state).await;

        let downloads = state.active_downloads.lock().await;
        assert!(
            downloads.contains_key("running"),
            "active downloads must never be dropped by session retention"
        );
    }
}

#[cfg(test)]
mod metadata_cache_tests {
    use super::*;
    use serde_json::json;

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

    #[tokio::test]
    async fn test_store_and_get_roundtrip() {
        let state = fresh_state();
        let payload = json!({ "id": "abc", "title": "Test" });

        store_cached_meta(
            &state,
            "https://www.youtube.com/watch?v=abc".into(),
            payload.clone(),
        )
        .await;

        let got = get_cached_meta(&state, "https://www.youtube.com/watch?v=abc").await;
        assert_eq!(got, Some(payload));
    }

    #[tokio::test]
    async fn test_expired_entry_not_returned() {
        let state = fresh_state();

        {
            let mut cache = state.metadata_cache.lock().await;
            cache.insert(
                "stale".to_string(),
                CachedMeta {
                    json: json!({ "id": "stale" }),
                    fetched_at: now_secs() - *META_CACHE_TTL_HOURS * 3600.0 - 1.0,
                },
            );
            cache.insert(
                "fresh".to_string(),
                CachedMeta {
                    json: json!({ "id": "fresh" }),
                    fetched_at: now_secs(),
                },
            );
        }

        assert!(get_cached_meta(&state, "stale").await.is_none());
        assert!(get_cached_meta(&state, "fresh").await.is_some());
    }

    #[tokio::test]
    async fn test_cleanup_purges_expired_entries() {
        let state = fresh_state();
        {
            let mut cache = state.metadata_cache.lock().await;
            cache.insert(
                "old".to_string(),
                CachedMeta {
                    json: json!({}),
                    fetched_at: now_secs() - *META_CACHE_TTL_HOURS * 3600.0 - 10.0,
                },
            );
            cache.insert(
                "new".to_string(),
                CachedMeta {
                    json: json!({}),
                    fetched_at: now_secs(),
                },
            );
        }

        cleanup_old_data(&state).await;

        let cache = state.metadata_cache.lock().await;
        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key("new"));
    }

    #[tokio::test]
    async fn test_store_caps_cache_at_limit() {
        let state = fresh_state();
        let total = META_CACHE_MAX_ENTRIES + 20;
        for i in 0..total {
            store_cached_meta(&state, format!("key-{}", i), json!({})).await;
        }

        let cache = state.metadata_cache.lock().await;
        assert_eq!(
            cache.len(),
            META_CACHE_MAX_ENTRIES,
            "cache must never grow beyond the entry limit"
        );
        assert!(!cache.contains_key("key-0"), "oldest entry must be evicted");
        assert!(
            cache.contains_key(&format!("key-{}", total - 1)),
            "newest entry must survive"
        );
    }

    #[test]
    fn test_eviction_removes_expired_first_then_oldest() {
        let now = now_secs();
        let ttl_secs = *META_CACHE_TTL_HOURS * 3600.0;
        let mut cache = HashMap::new();
        for i in 0..META_CACHE_MAX_ENTRIES {
            cache.insert(
                format!("expired-{}", i),
                CachedMeta {
                    json: json!({}),
                    fetched_at: now - ttl_secs - 1.0 - i as f64,
                },
            );
        }

        evict_metadata_cache(&mut cache);
        assert!(cache.is_empty(), "all expired entries should be purged");

        for i in 0..META_CACHE_MAX_ENTRIES {
            cache.insert(
                format!("k{}", i),
                CachedMeta {
                    json: json!({}),
                    fetched_at: now,
                },
            );
        }
        cache.insert(
            "newcomer".to_string(),
            CachedMeta {
                json: json!({}),
                fetched_at: now + 1.0,
            },
        );

        evict_metadata_cache(&mut cache);

        assert_eq!(cache.len(), META_CACHE_MAX_ENTRIES);
        assert!(
            cache.contains_key("newcomer"),
            "fresh entry must not be evicted"
        );
    }
}

#[cfg(test)]
mod password_verification_tests {
    use super::*;

    #[test]
    fn test_is_bcrypt_hash_detection() {
        let hash = bcrypt::hash("secret", 4).unwrap();
        assert!(is_bcrypt_hash(&hash));
        assert!(!is_bcrypt_hash("plain-secret"));
        assert!(!is_bcrypt_hash("$2a$tooshort"));
        assert!(!is_bcrypt_hash(""));
    }

    #[test]
    fn test_verify_password_bcrypt_path() {
        // Cost 4 is the bcrypt minimum and keeps the test fast
        let hash = bcrypt::hash("correct horse battery staple", 4).unwrap();
        assert!(verify_password_against(
            &hash,
            "correct horse battery staple"
        ));
        assert!(!verify_password_against(&hash, "wrong password"));
        assert!(!verify_password_against(&hash, ""));
    }

    #[test]
    fn test_verify_password_legacy_plaintext_path() {
        // Legacy .env values are plain strings compared exactly as before
        assert!(verify_password_against("legacy-pw", "legacy-pw"));
        assert!(!verify_password_against("legacy-pw", "Legacy-pw"));
        assert!(!verify_password_against("legacy-pw", "legacy-pwx"));
        assert!(!verify_password_against("legacy-pw", ""));
    }

    #[tokio::test]
    async fn test_api_rate_limit_window_slides() {
        let buckets: Mutex<HashMap<String, Vec<f64>>> = Mutex::new(HashMap::new());

        for _ in 0..3 {
            assert!(consume_rate_limit(&buckets, "analyze:1.2.3.4", 3, API_WINDOW_SECS).await);
        }
        // Budget exhausted
        assert!(!consume_rate_limit(&buckets, "analyze:1.2.3.4", 3, API_WINDOW_SECS).await);

        // Simulate the window sliding past all recorded timestamps
        let now = now_secs();
        let mut guard = buckets.lock().await;
        guard.insert("analyze:1.2.3.4".into(), vec![now - API_WINDOW_SECS - 1.0]);
        drop(guard);

        assert!(
            consume_rate_limit(&buckets, "analyze:1.2.3.4", 3, API_WINDOW_SECS).await,
            "expired timestamps must free the budget again"
        );
    }

    #[test]
    fn test_rate_limit_keys_are_independent() {
        // Compile-time sanity: distinct keys never share a bucket entry.
        let analyze_key = format!("analyze:{ip}", ip = "9.9.9.9");
        let download_key = format!("download:{ip}", ip = "9.9.9.9");
        assert_ne!(analyze_key, download_key);
    }
}
