use once_cell::sync::Lazy;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    time::{interval, sleep},
};
use tracing::{error, info, warn};

use crate::metadata;
use crate::state::{
    get_cached_meta, now_secs, store_cached_meta, SharedState, DOWNLOAD_DIR, DOWNLOAD_SEMAPHORE,
};
use crate::tags;

async fn enrich_audio(
    state: &SharedState,
    download_id: &str,
    path: &Path,
    video_info: &serde_json::Value,
    want_lyrics: bool,
) -> Option<metadata::Lyrics> {
    let (artist, track, album) = metadata::track_meta(video_info);
    if track.is_empty() {
        return None;
    }

    {
        let mut downloads = state.active_downloads.lock().await;
        if let Some(info) = downloads.get_mut(download_id) {
            info.speed = Some("Metadaten werden vervollständigt …".to_string());
        }
    }

    let artwork = match metadata::fetch_album_art(&artist, &track).await {
        Some(jpeg) => Some(jpeg),
        None => {
            let thumb = video_info
                .get("thumbnail")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            metadata::fetch_image(thumb).await
        }
    };

    match artwork {
        Some(jpeg) => match tags::embed_artwork(path, &jpeg) {
            Ok(()) => info!(
                "Download {}: embedded album art ({} - {})",
                download_id, artist, track
            ),
            Err(e) => warn!("Download {}: album art embed failed: {}", download_id, e),
        },
        None => warn!(
            "Download {}: no album art found for '{} - {}'",
            download_id, artist, track
        ),
    }

    if !want_lyrics {
        return None;
    }

    let duration = video_info.get("duration").and_then(|d| d.as_f64());
    match metadata::fetch_lyrics(&artist, &track, &album, duration).await {
        Some(lyrics) => {
            let plain = lyrics
                .plain
                .clone()
                .or_else(|| lyrics.synced.as_deref().map(metadata::strip_lrc_timestamps));
            if let Some(p) = &plain {
                if let Err(e) = tags::embed_lyrics(path, p) {
                    warn!("Download {}: lyrics embed failed: {}", download_id, e);
                }
            }
            Some(lyrics)
        }
        None => {
            info!("Download {}: no lyrics found for '{}'", download_id, track);
            None
        }
    }
}

// Pre-compiled regex for progress parsing (captures percentage, speed, and ETA)
pub static PROGRESS_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\[download\]\s+(\d+\.?\d*)%(?:\s+of\s+(?:~)?(\d+\.?\d*)(KiB|MiB|GiB|unknown))?(?:\s+at\s+(\d+\.?\d*)(KiB|MiB|GiB)/s)?(?:\s+ETA\s+(\d+:?\d*))?").unwrap()
});

pub const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

pub fn get_audio_multiplier(acodec: &str, ext: &str) -> f64 {
    let lower_acodec = acodec.to_lowercase();
    let lower_ext = ext.to_lowercase();
    if lower_acodec.contains("opus") || lower_ext.contains("opus") {
        1.4
    } else if lower_acodec.contains("aac")
        || lower_acodec.contains("mp4a")
        || lower_ext.contains("m4a")
    {
        1.1
    } else if lower_acodec.contains("vorbis") || lower_ext.contains("ogg") {
        1.0
    } else if lower_acodec.contains("mp3") || lower_ext.contains("mp3") {
        0.8
    } else {
        0.7
    }
}

pub fn map_audio_format_name(acodec: &str, ext: &str) -> String {
    let lower_acodec = acodec.to_lowercase();
    let lower_ext = ext.to_lowercase();
    if lower_acodec.contains("opus") || lower_ext.contains("opus") {
        "opus".to_string()
    } else if lower_acodec.contains("aac")
        || lower_acodec.contains("mp4a")
        || lower_ext.contains("m4a")
    {
        "m4a".to_string()
    } else if lower_acodec.contains("mp3") || lower_ext.contains("mp3") {
        "mp3".to_string()
    } else if lower_acodec.contains("flac") || lower_ext.contains("flac") {
        "flac".to_string()
    } else {
        ext.to_string()
    }
}

pub fn sanitize_filename(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_alphanumeric() || c == '.' || c == '_' || c == '-' || c == '(' || c == ')' {
            result.push(c);
        } else if c == ' ' {
            result.push('_');
        }
    }
    while result.ends_with('_') {
        result.pop();
    }
    result
}

pub async fn get_free_space_bytes(path: &std::path::Path) -> Option<u64> {
    let out = Command::new("df")
        .arg("-B1")
        .arg(path)
        .output()
        .await
        .ok()?;

    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().collect();
        if lines.len() >= 2 {
            let cols: Vec<&str> = lines[1].split_whitespace().collect();
            if cols.len() >= 4 {
                if let Ok(bytes) = cols[3].parse::<u64>() {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

/// Check and update yt-dlp to the latest version
pub async fn update_yt_dlp() {
    info!("Checking for yt-dlp updates...");

    let output = tokio::time::timeout(
        Duration::from_secs(180),
        Command::new("yt-dlp")
            .args(["-U"])
            .kill_on_drop(true)
            .output(),
    )
    .await;

    match output {
        Ok(Ok(result)) => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);

            if result.status.success() {
                if stdout.contains("up to date") || stdout.is_empty() {
                    info!("yt-dlp is already up to date");
                } else {
                    info!("yt-dlp updated successfully");
                }
            } else {
                warn!("yt-dlp update check failed: {} {}", stdout, stderr);
            }
        }
        Ok(Err(e)) => {
            warn!("Failed to run yt-dlp -U: {}", e);
        }
        Err(_) => {
            warn!("yt-dlp update timed out");
        }
    }
}

/// Periodically check for yt-dlp updates (every 24 hours)
pub async fn periodic_yt_dlp_update() {
    let mut interval = interval(Duration::from_secs(SECONDS_PER_DAY));

    // The boot-time check in main() already runs an update; consume the
    // immediate first tick so we don't run two concurrent 'yt-dlp -U'.
    interval.tick().await;

    loop {
        interval.tick().await;
        info!("Running periodic yt-dlp update check...");
        update_yt_dlp().await;
    }
}

pub async fn download_task(
    state: SharedState,
    download_id: String,
    url: String,
    format_type: String,
    quality: String,
    want_lyrics: bool,
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

    let result = execute_download(&state, &download_id, &url, &format_type, &quality, want_lyrics).await;

    if let Err(e) = result {
        error!("Download {} failed: {}", download_id, e);
        let mut downloads = state.active_downloads.lock().await;
        if let Some(info) = downloads.get_mut(&download_id) {
            // An aborted download was killed intentionally; keep its status.
            if info.status != "aborted" {
                info.status = "error".to_string();
                info.error = Some(e);
            }
        }
    }

    drop(permit);
}

pub async fn execute_download(
    state: &SharedState,
    download_id: &str,
    url: &str,
    format_type: &str,
    quality: &str,
    want_lyrics: bool,
) -> Result<(), String> {
    let video_info: serde_json::Value = if let Some(info) = get_cached_meta(state, url).await {
        info!("Download {} using cached metadata for {}", download_id, url);
        info
    } else {
        let info_output = Command::new("yt-dlp")
            .args([
                "--quiet",
                "--no-warnings",
                "--dump-json",
                "--no-download",
                url,
            ])
            .kill_on_drop(true)
            .output();

        let info_output = match tokio::time::timeout(Duration::from_secs(90), info_output).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(format!("Failed to get video info: {}", e)),
            Err(_) => return Err("Zeitüberschreitung beim Abrufen der Video-Informationen".to_string()),
        };

        if !info_output.status.success() {
            return Err("Failed to analyze video".to_string());
        }

        let parsed: serde_json::Value = serde_json::from_slice(&info_output.stdout)
            .map_err(|e| format!("Failed to parse video info: {}", e))?;
        store_cached_meta(state, url.to_string(), parsed.clone()).await;
        parsed
    };

    let filesize = video_info
        .get("filesize")
        .and_then(|fs| fs.as_u64())
        .or_else(|| video_info.get("filesize_approx").and_then(|fs| fs.as_u64()))
        .unwrap_or(0);

    if filesize > 0 {
        if let Some(free_space) = get_free_space_bytes(&DOWNLOAD_DIR).await {
            let required_space = (filesize as f64 * 1.2) as u64;
            if free_space < required_space {
                return Err(format!(
                    "Nicht genügend Speicherplatz auf dem Pi! Freier Speicher: {} MB, Erforderlich (inkl. Puffer): {} MB",
                    free_space / (1024 * 1024),
                    required_space / (1024 * 1024)
                ));
            }
        }
    }

    let title = video_info
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("download");

    let _file_name = format!("{}.{}", sanitize_filename(title), "mp4");

    let mut args = vec![
        "--quiet",
        "--no-warnings",
        "--newline",
        "--progress",
        "--embed-metadata",
        "--embed-chapters",
        "--sponsorblock-remove",
        "sponsor",
    ];

    let format_arg;
    let mut extract_audio = false;

    if format_type == "audio" {
        extract_audio = true;
        if quality == "best" {
            format_arg = "bestaudio/best".to_string();
        } else {
            format_arg = quality.to_string();
        }
    } else {
        // yt-dlp's --embed-thumbnail needs python-mutagen for ogg/opus and is
        // redundant for audio anyway: enrich_audio() embeds real album art.
        args.push("--embed-thumbnail");
        args.push("--convert-thumbnails");
        args.push("jpg");
        if quality == "best" {
            format_arg = "bestvideo[ext=mp4]+bestaudio[ext=m4a]/bestvideo+bestaudio/best".to_string();
        } else {
            let parts: Vec<&str> = quality.split('-').collect();
            if parts.len() == 2 {
                let height_str: String = parts[0].chars().filter(|c| c.is_ascii_digit()).collect();
                let ext = parts[1];
                format_arg = format!(
                    "bestvideo[height<={}][ext=mp4]+bestaudio[ext=m4a]/bestvideo[height<={}][ext={}]+bestaudio/bestvideo[height<={}]+bestaudio/best",
                    height_str, height_str, ext, height_str
                );
            } else {
                format_arg = "bestvideo[ext=mp4]+bestaudio[ext=m4a]/bestvideo+bestaudio/best".to_string();
            }
        }
        // Merged video+audio streams must land in an mp4 container; without
        // this yt-dlp defaults to mkv.
        args.push("--merge-output-format");
        args.push("mp4");
    }

    args.push("-f");
    args.push(&format_arg);

    let output_template = DOWNLOAD_DIR.join(format!("%(title)s-[{}].%(ext)s", download_id));
    let output_template_str = output_template.to_str().ok_or("Invalid output path")?;
    args.push("-o");
    args.push(output_template_str);

    if extract_audio {
        args.push("--extract-audio");
        if quality == "best" {
            // "Beste Qualität" delivers a universally playable MP3 (V0).
            args.push("--audio-format");
            args.push("mp3");
            args.push("--audio-quality");
            args.push("0");
        } else {
            // Explicit codec presets keep their native lossless-of-source
            // container (opus/m4a/...), no re-encode.
            args.push("--audio-format");
            args.push("best");
        }
    }

    args.push(url);

    info!("Executing yt-dlp with args: {:?}", args);

    let mut child = Command::new("yt-dlp")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to spawn yt-dlp: {}", e))?;

    let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;

    // Register the child process for Graceful Shutdown
    {
        let mut children = state.active_children.lock().await;
        children.insert(download_id.to_string(), child);
    }

    // Re-acquire the child to read its progress
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                if let Some(cap) = PROGRESS_REGEX.captures(&line) {
                    if let Some(percent_match) = cap.get(1) {
                        if let Ok(percent) = percent_match.as_str().parse::<f64>() {
                            let speed = if let (Some(speed_val), Some(speed_unit)) =
                                (cap.get(4), cap.get(5))
                            {
                                Some(format!("{} {}/s", speed_val.as_str(), speed_unit.as_str()))
                            } else {
                                None
                            };

                            let eta = cap.get(6).map(|m| m.as_str().to_string());

                            let mut downloads = state.active_downloads.lock().await;
                            if let Some(info) = downloads.get_mut(download_id) {
                                info.progress = percent;
                                info.last_activity = now_secs();
                                info.speed = speed;
                                info.eta = eta;
                            }
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    // Get the child back out of our registry to wait/handle its exit
    let mut child = {
        let mut children = state.active_children.lock().await;
        children
            .remove(download_id)
            .ok_or("Child process untracked unexpectedly")?
    };

    let stderr = child.stderr.take();
    let status = child
        .wait()
        .await
        .map_err(|e| format!("Failed to wait for process: {}", e))?;

    if !status.success() {
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

    let mut downloaded_file: Option<PathBuf> = None;
    let id_marker = format!("[{}]", download_id);

    sleep(Duration::from_millis(500)).await;

    match fs::read_dir(&*DOWNLOAD_DIR).await {
        Ok(mut entries) => {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.contains(&id_marker)
                        && !name.ends_with(".part")
                        && !name.ends_with(".ytdl")
                    {
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
            error!(
                "Could not find downloaded file with ID marker: {}",
                id_marker
            );
            return Err("Downloaded file not found".to_string());
        }
    };

    let final_file_name = final_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&_file_name)
        .to_string();

    if format_type == "audio" {
        let lyrics =
            enrich_audio(state, download_id, &final_path, &video_info, want_lyrics).await;
        if let Some(l) = lyrics {
            let plain = l.plain.clone().or_else(|| {
                l.synced.as_deref().map(metadata::strip_lrc_timestamps)
            });
            let mut downloads = state.active_downloads.lock().await;
            if let Some(info) = downloads.get_mut(download_id) {
                info.lyrics_plain = plain;
                info.lyrics_synced = l.synced;
            }
        }
    }

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

pub async fn playlist_download_task(
    state: SharedState,
    download_id: String,
    urls: Vec<String>,
    format_type: String,
    quality: String,
    want_lyrics: bool,
) {
    info!("Playlist download {} waiting for semaphore...", download_id);
    let permit = DOWNLOAD_SEMAPHORE.acquire().await;
    {
        let mut downloads = state.active_downloads.lock().await;
        if let Some(info) = downloads.get_mut(&download_id) {
            info.status = "processing".to_string();
        }
    }

    info!(
        "Playlist download {} starting processing of {} videos",
        download_id,
        urls.len()
    );

    let result = execute_playlist_download(
        &state,
        &download_id,
        &urls,
        &format_type,
        &quality,
        want_lyrics,
    )
    .await;

    if let Err(e) = result {
        error!("Playlist download {} failed: {}", download_id, e);
        let mut downloads = state.active_downloads.lock().await;
        if let Some(info) = downloads.get_mut(&download_id) {
            // An aborted download was killed intentionally; keep its status.
            if info.status != "aborted" {
                info.status = "error".to_string();
                info.error = Some(e);
            }
        }
    }

    drop(permit);
}

pub async fn execute_playlist_download(
    state: &SharedState,
    download_id: &str,
    urls: &[String],
    format_type: &str,
    quality: &str,
    want_lyrics: bool,
) -> Result<(), String> {
    let temp_dir = DOWNLOAD_DIR.join(format!("playlist-temp-{}", download_id));
    fs::create_dir_all(&temp_dir)
        .await
        .map_err(|e| format!("Fehler beim Erstellen des Temp-Verzeichnisses: {}", e))?;

    let total_videos = urls.len();
    let mut downloaded_files = Vec::new();
    let mut lyric_files = Vec::new();

    for (index, url) in urls.iter().enumerate() {
        // Stop launching new videos once the download was aborted.
        let aborted = {
            let downloads = state.active_downloads.lock().await;
            match downloads.get(download_id) {
                Some(info) => info.status == "aborted",
                None => true,
            }
        };
        if aborted {
            info!("Playlist download {} aborted, stopping", download_id);
            break;
        }

        {
            let mut downloads = state.active_downloads.lock().await;
            if let Some(info) = downloads.get_mut(download_id) {
                info.speed = Some(format!("Video {}/{}", index + 1, total_videos));
            }
        }

        let video_info: serde_json::Value = if let Some(info) = get_cached_meta(state, url).await {
            info!("Playlist {} using cached metadata for {}", download_id, url);
            info
        } else {
            let info_output = Command::new("yt-dlp")
                .args([
                    "--quiet",
                    "--no-warnings",
                    "--dump-json",
                    "--no-download",
                    url,
                ])
                .kill_on_drop(true)
                .output();

            let info_output = match tokio::time::timeout(Duration::from_secs(60), info_output).await
            {
                Ok(Ok(out)) => out,
                Ok(Err(e)) => {
                    warn!("Failed to analyze playlist video at {}: {}", url, e);
                    continue;
                }
                Err(_) => {
                    warn!("Timeout analyzing playlist video at {}", url);
                    continue;
                }
            };

            if !info_output.status.success() {
                warn!("Failed to analyze playlist video at {}", url);
                continue;
            }

            let parsed: serde_json::Value =
                serde_json::from_slice(&info_output.stdout)
                    .map_err(|e| format!("Failed to parse video info: {}", e))?;
            store_cached_meta(state, url.to_string(), parsed.clone()).await;
            parsed
        };

        let title = video_info
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("download");

        let ext = if format_type == "audio" { "mp3" } else { "mp4" };
        let out_file_template = temp_dir.join(format!("video-{}.%(ext)s", index));

        let mut args = vec![
            "--quiet",
            "--no-warnings",
            "--newline",
            "--progress",
            "--embed-metadata",
            "--embed-chapters",
            "--sponsorblock-remove",
            "sponsor",
        ];

        let format_arg;
        let mut extract_audio = false;

        if format_type == "audio" {
            extract_audio = true;
            if quality == "best" {
                format_arg = "bestaudio/best".to_string();
            } else {
                format_arg = quality.to_string();
            }
        } else {
            args.push("--embed-thumbnail");
            args.push("--convert-thumbnails");
            args.push("jpg");
            if quality == "best" {
                format_arg = "bestvideo[ext=mp4]+bestaudio[ext=m4a]/bestvideo+bestaudio/best".to_string();
            } else {
                let parts: Vec<&str> = quality.split('-').collect();
                if parts.len() == 2 {
                    let height_str: String =
                        parts[0].chars().filter(|c| c.is_ascii_digit()).collect();
                    let ext = parts[1];
                    format_arg = format!("bestvideo[height<={}][ext=mp4]+bestaudio[ext=m4a]/bestvideo[height<={}][ext={}]+bestaudio/bestvideo[height<={}]+bestaudio/best", height_str, height_str, ext, height_str);
                } else {
                    format_arg = "bestvideo[ext=mp4]+bestaudio[ext=m4a]/bestvideo+bestaudio/best".to_string();
                }
            }
            args.push("--merge-output-format");
            args.push("mp4");
        }

        args.push("-f");
        args.push(&format_arg);
        args.push("-o");
        let out_file_template_str = out_file_template.to_str().ok_or("Invalid output path")?;
        args.push(out_file_template_str);

        if extract_audio {
            args.push("--extract-audio");
            if quality == "best" {
                args.push("--audio-format");
                args.push("mp3");
                args.push("--audio-quality");
                args.push("0");
            } else {
                args.push("--audio-format");
                args.push("best");
            }
        }

        args.push(url);

        let mut child = Command::new("yt-dlp")
            .args(&args)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Failed to spawn yt-dlp: {}", e))?;

        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;

        // Register child process for graceful shutdown
        {
            let mut children = state.active_children.lock().await;
            children.insert(format!("{}-{}", download_id, index), child);
        }

        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if let Some(cap) = PROGRESS_REGEX.captures(&line) {
                        if let Some(percent_match) = cap.get(1) {
                            if let Ok(percent) = percent_match.as_str().parse::<f64>() {
                                let overall_pct =
                                    ((index as f64) * 100.0 + percent) / (total_videos as f64);

                                let speed = if let (Some(speed_val), Some(speed_unit)) =
                                    (cap.get(4), cap.get(5))
                                {
                                    Some(format!(
                                        "Video {}/{} • {} {}/s",
                                        index + 1,
                                        total_videos,
                                        speed_val.as_str(),
                                        speed_unit.as_str()
                                    ))
                                } else {
                                    Some(format!("Video {}/{}", index + 1, total_videos))
                                };

                                let eta = cap.get(6).map(|m| m.as_str().to_string());

                                let mut downloads = state.active_downloads.lock().await;
                                if let Some(info) = downloads.get_mut(download_id) {
                                    info.progress = overall_pct;
                                    info.last_activity = now_secs();
                                    info.speed = speed;
                                    info.eta = eta;
                                }
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }

        // Clean up from active_children registry
        let mut child = {
            let mut children = state.active_children.lock().await;
            children
                .remove(&format!("{}-{}", download_id, index))
                .ok_or("Playlist child process untracked unexpectedly")?
        };

        let _ = child.wait().await;

        if let Ok(mut entries) = fs::read_dir(&temp_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_file() {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.starts_with(&format!("video-{}", index))
                        && !name.ends_with(".part")
                        && !name.ends_with(".ytdl")
                    {
                        let file_ext =
                            path.extension().and_then(|e| e.to_str()).unwrap_or(ext);
                        let sanitized_title = sanitize_filename(title);
                        let target_path =
                            temp_dir.join(format!("{}.{}", sanitized_title, file_ext));
                        let _ = fs::rename(&path, &target_path).await;
                        downloaded_files.push(target_path);

                        if format_type == "audio" && want_lyrics {
                            if let Some(l) = enrich_audio(
                                state,
                                download_id,
                                &temp_dir.join(format!("{}.{}", sanitized_title, file_ext)),
                                &video_info,
                                true,
                            )
                            .await
                            {
                                let text = l
                                    .synced
                                    .clone()
                                    .or_else(|| l.plain.clone())
                                    .unwrap_or_default();
                                if !text.is_empty() {
                                    let lrc_path = temp_dir
                                        .join(format!("{}.lrc", sanitized_title));
                                    if fs::write(&lrc_path, &text).await.is_ok() {
                                        lyric_files.push(lrc_path);
                                    }
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }
    }

    // If the download was aborted mid-run, discard everything collected so far.
    {
        let downloads = state.active_downloads.lock().await;
        let is_aborted = match downloads.get(download_id) {
            Some(info) => info.status == "aborted",
            None => true,
        };
        if is_aborted {
            drop(downloads);
            let _ = fs::remove_dir_all(&temp_dir).await;
            info!("Playlist download {} aborted, discarding partial results", download_id);
            return Ok(());
        }
    }

    if downloaded_files.is_empty() {
        let _ = fs::remove_dir_all(&temp_dir).await;
        return Err("Keines der Playlist-Videos konnte heruntergeladen werden.".to_string());
    }

    let zip_name = format!("playlist-[{}].zip", download_id);
    let zip_path = DOWNLOAD_DIR.join(&zip_name);

    {
        let file =
            std::fs::File::create(&zip_path).map_err(|e| format!("Failed to create zip: {}", e))?;
        let mut zip = zip::ZipWriter::new(file);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        for file_path in downloaded_files.iter().chain(lyric_files.iter()) {
            if let Some(name) = file_path.file_name().and_then(|n| n.to_str()) {
                zip.start_file(name, options)
                    .map_err(|e| format!("Failed to add file to zip: {}", e))?;
                let mut f = std::fs::File::open(file_path)
                    .map_err(|e| format!("Failed to open file for zipping: {}", e))?;
                std::io::copy(&mut f, &mut zip)
                    .map_err(|e| format!("Failed to zip content: {}", e))?;
            }
        }
        zip.finish()
            .map_err(|e| format!("Failed to finish zip: {}", e))?;
    }

    let _ = fs::remove_dir_all(&temp_dir).await;

    {
        let mut downloads = state.active_downloads.lock().await;
        if let Some(info) = downloads.get_mut(download_id) {
            info.status = "completed".to_string();
            info.progress = 100.0;
            info.file_path = Some(zip_path);
            info.file_name = Some(zip_name);
            info.speed = None;
            info.eta = None;
        }
    }

    Ok(())
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
        assert_eq!(sanitize_filename("héllo wörld"), "héllo_wörld");
        assert_eq!(sanitize_filename("日本語"), "日本語");
        assert_eq!(sanitize_filename("hello🎉world"), "helloworld");
    }

    #[test]
    fn test_map_audio_format_name() {
        assert_eq!(map_audio_format_name("opus", "webm"), "opus");
        assert_eq!(map_audio_format_name("mp4a.40.2", "m4a"), "m4a");
        assert_eq!(map_audio_format_name("mp3", "mp3"), "mp3");
        assert_eq!(map_audio_format_name("unknown_codec", "wav"), "wav");
    }

    #[test]
    fn test_get_audio_multiplier() {
        assert_eq!(get_audio_multiplier("opus", "webm"), 1.4);
        assert_eq!(get_audio_multiplier("mp4a.40.2", "m4a"), 1.1);
        assert_eq!(get_audio_multiplier("mp3", "mp3"), 0.8);
        assert_eq!(get_audio_multiplier("unknown_codec", "wav"), 0.7);
    }

    #[test]
    fn test_progress_regex_matches() {
        let test_cases = vec![
            ("[download]  50.5%", Some(("50.5", None, None, None))),
            ("[download] 100%", Some(("100", None, None, None))),
            ("[download]   0%", Some(("0", None, None, None))),
            (
                "[download]  12.34% of 100MiB",
                Some(("12.34", Some("100"), Some("MiB"), None)),
            ),
            (
                "[download]  50% of ~123.45MiB",
                Some(("50", Some("123.45"), Some("MiB"), None)),
            ),
            (
                "[download]  50% of 100MiB at 5.67MiB/s",
                Some(("50", Some("100"), Some("MiB"), Some("5.67"))),
            ),
            (
                "[download]  45.2% of  123.45MiB at    5.67MiB/s ETA 00:15",
                Some(("45.2", Some("123.45"), Some("MiB"), Some("5.67"))),
            ),
            (
                "[download]  80% of ~500MiB at 10.5MiB/s ETA 01:30",
                Some(("80", Some("500"), Some("MiB"), Some("10.5"))),
            ),
        ];

        for (input, expected) in test_cases {
            let caps = PROGRESS_REGEX.captures(input);
            if let Some((exp_pct, exp_size_val, exp_size_unit, exp_speed)) = expected {
                assert!(caps.is_some(), "Should match: {}", input);
                let caps = caps.unwrap();
                assert_eq!(
                    caps.get(1).unwrap().as_str(),
                    exp_pct,
                    "Percentage mismatch for: {}",
                    input
                );

                if let Some(exp_val) = exp_size_val {
                    assert!(
                        caps.get(2).is_some(),
                        "Should have size value for: {}",
                        input
                    );
                    assert_eq!(
                        caps.get(2).unwrap().as_str(),
                        exp_val,
                        "Size value mismatch for: {}",
                        input
                    );
                }

                if let Some(exp_unit) = exp_size_unit {
                    assert!(
                        caps.get(3).is_some(),
                        "Should have size unit for: {}",
                        input
                    );
                    assert_eq!(
                        caps.get(3).unwrap().as_str(),
                        exp_unit,
                        "Size unit mismatch for: {}",
                        input
                    );
                }

                if let Some(exp_spd) = exp_speed {
                    assert!(caps.get(4).is_some(), "Should have speed for: {}", input);
                    assert_eq!(
                        caps.get(4).unwrap().as_str(),
                        exp_spd,
                        "Speed mismatch for: {}",
                        input
                    );
                }
            }
        }
    }

    #[test]
    fn test_progress_regex_no_match() {
        let no_match_cases = vec!["[info] Downloading", "50% complete", "", "[download]"];

        for input in no_match_cases {
            assert!(
                PROGRESS_REGEX.captures(input).is_none(),
                "Should not match: {}",
                input
            );
        }
    }

    #[test]
    fn test_temp_file_naming() {
        let file_name = "video.mp4";
        let temp_name = format!("{}.part", file_name);
        assert_eq!(temp_name, "video.mp4.part");

        let file_name = "song.mp3";
        let temp_name = format!("{}.part", file_name);
        assert_eq!(temp_name, "song.mp3.part");
    }

    #[test]
    fn test_unique_filename_generation() {
        let safe_title = "my_video";
        let ext = "mp4";
        let timestamp = 1234567890u64;
        let unique_name = format!("{}_{}.{}", safe_title, timestamp, ext);
        assert_eq!(unique_name, "my_video_1234567890.mp4");
    }
}
