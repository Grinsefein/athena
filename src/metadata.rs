use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

const ITUNES_SEARCH_URL: &str = "https://itunes.apple.com/search";
const LRCLIB_GET_URL: &str = "https://lrclib.net/api/get";
const LRCLIB_SEARCH_URL: &str = "https://lrclib.net/api/search";

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(concat!("AthenaMedia/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .expect("failed to build HTTP client")
});

static JUNK_SUFFIX_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\s*[\(\[\{]\s*(official\s+)?(music\s+)?(video|audio|lyric[s]?\s*video|lyrics|visualizer|mv|hd|hq|4k|remaster(ed)?(\s*\d{4})?|explicit|feat\.?.*|ft\.?.*)\s*[\)\]\}]").unwrap()
});

static FEAT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\s+(feat\.?|ft\.?|featuring|mit)\s+.+$").unwrap());

static LRC_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\[[0-9]{1,2}:[0-9]{1,2}(?:[.:][0-9]{1,3})?\]").unwrap());

#[derive(Clone, Debug, PartialEq)]
pub struct Lyrics {
    pub plain: Option<String>,
    pub synced: Option<String>,
}

/// Extract the most reliable (artist, track, album) triple from yt-dlp info.
/// Falls back to splitting common "Artist - Title" title patterns.
pub fn track_meta(info: &Value) -> (String, String, String) {
    let title = info
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let artist = ["artist", "creator", "uploader"]
        .iter()
        .find_map(|k| info.get(k).and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    let track = info
        .get("track")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    let album = info
        .get("album")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    match (artist, track) {
        (Some(a), Some(t)) => (a, t, album.unwrap_or_default()),
        (Some(a), None) => (a, strip_junk(&title), album.unwrap_or_default()),
        (None, Some(t)) => (String::new(), t, album.unwrap_or_default()),
        (None, None) => match split_artist_title(&title) {
            Some((a, t)) => (a, t, album.unwrap_or_default()),
            None => (String::new(), strip_junk(&title), album.unwrap_or_default()),
        },
    }
}

/// Split a raw title like "Artist - Title" into its parts.
pub fn split_artist_title(title: &str) -> Option<(String, String)> {
    let (artist, rest) = title.split_once(" - ")?;
    let artist = artist.trim();
    let track = strip_junk(rest);
    if artist.is_empty() || track.is_empty() {
        return None;
    }
    Some((artist.to_string(), track))
}

/// Remove typical YouTube clutter from a track name.
pub fn strip_junk(title: &str) -> String {
    let mut out = JUNK_SUFFIX_RE.replace_all(title, "").to_string();
    out = FEAT_RE.replace_all(&out, "").to_string();
    out.trim()
        .trim_matches(|c: char| c == '-' || c == '_' || c == '|')
        .trim()
        .to_string()
}

/// iTunes artwork URLs are served at 100x100; swapping the size suffix yields
/// up to 600x600 without another API roundtrip.
pub fn upscale_artwork_url(url: &str) -> String {
    url.replace("100x100bb", "600x600bb")
}

pub fn has_lrc_timestamps(line: &str) -> bool {
    LRC_LINE_RE.is_match(line.trim())
}

async fn fetch_album_art_url(artist: &str, track: &str) -> Option<String> {
    let term = format!("{} {}", artist, track).trim().to_string();
    if term.is_empty() {
        return None;
    }

    let resp = CLIENT
        .get(ITUNES_SEARCH_URL)
        .query(&[
            ("term", term.as_str()),
            ("media", "music"),
            ("entity", "song"),
            ("limit", "5"),
        ])
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let json: Value = resp.json().await.ok()?;
    let results = json.get("results")?.as_array()?;

    for hit in results {
        let hit_track = hit.get("trackName").and_then(|t| t.as_str()).unwrap_or("");
        let hit_artist = hit.get("artistName").and_then(|a| a.as_str()).unwrap_or("");
        if normalize_cmp(hit_track) != normalize_cmp(track) && !normalize_cmp(track).is_empty() {
            continue;
        }
        if !artist.is_empty() && normalize_cmp(hit_artist) != normalize_cmp(artist) {
            continue;
        }
        if let Some(url) = hit.get("artworkUrl100").and_then(|u| u.as_str()) {
            return Some(upscale_artwork_url(url));
        }
    }

    results
        .iter()
        .find_map(|hit| hit.get("artworkUrl100").and_then(|u| u.as_str()))
        .map(upscale_artwork_url)
}

fn normalize_cmp(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

async fn fetch_image_bytes(url: &str) -> Option<Vec<u8>> {
    let resp = CLIENT.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?.to_vec();
    let img = image::load_from_memory(&bytes).ok()?;
    let rgb = img.to_rgb8();
    let mut jpeg = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
    rgb.write_with_encoder(encoder).ok()?;
    Some(jpeg)
}

/// Download and JPEG-normalize an arbitrary image URL (used for the video
/// thumbnail fallback when no real album art is available).
pub async fn fetch_image(url: &str) -> Option<Vec<u8>> {
    if url.is_empty() {
        return None;
    }
    fetch_image_bytes(url).await
}

async fn query_lrclib(
    artist: &str,
    track: &str,
    album: &str,
    duration: Option<f64>,
) -> Option<Value> {
    if track.is_empty() {
        return None;
    }

    let mut params: Vec<(&str, String)> = vec![
        ("track_name", track.to_string()),
        ("artist_name", artist.to_string()),
    ];
    if !album.is_empty() {
        params.push(("album_name", album.to_string()));
    }
    if let Some(d) = duration.filter(|d| *d > 0.0) {
        params.push(("duration", format!("{}", d.round() as u64)));
    }

    let resp = CLIENT
        .get(LRCLIB_GET_URL)
        .query(&params)
        .send()
        .await
        .ok()?;
    if resp.status().is_success() {
        if let Ok(json) = resp.json::<Value>().await {
            if !json.is_null() {
                return Some(json);
            }
        }
    }

    let resp = CLIENT
        .get(LRCLIB_SEARCH_URL)
        .query(
            &params
                .iter()
                .filter(|(k, _)| *k != "duration" && *k != "album_name")
                .map(|(k, v)| (*k, v.as_str()))
                .collect::<Vec<_>>(),
        )
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let list = resp.json::<Value>().await.ok()?;
    list.as_array().and_then(|arr| arr.first().cloned())
}

fn lyrics_from_entry(entry: &Value) -> Option<Lyrics> {
    let plain = entry
        .get("plainLyrics")
        .and_then(|p| p.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let synced = entry
        .get("syncedLyrics")
        .and_then(|p| p.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    if plain.is_none() && synced.is_none() {
        None
    } else {
        Some(Lyrics { plain, synced })
    }
}

/// Fetch real album art (iTunes Search API, no key required).
pub async fn fetch_album_art(artist: &str, track: &str) -> Option<Vec<u8>> {
    let url = fetch_album_art_url(artist, track).await?;
    fetch_image_bytes(&url).await
}

/// Fetch lyrics (lrclib.net, no key required). Returns plain and/or synced LRC.
pub async fn fetch_lyrics(
    artist: &str,
    track: &str,
    album: &str,
    duration: Option<f64>,
) -> Option<Lyrics> {
    let entry = query_lrclib(artist, track, album, duration).await?;
    lyrics_from_entry(&entry)
}

/// Convert synced LRC text to a timestamp-free plain version.
pub fn strip_lrc_timestamps(synced: &str) -> String {
    synced
        .lines()
        .map(|line| LRC_LINE_RE.replace(line.trim(), "").trim().to_string())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_strip_junk_removes_official_markers() {
        assert_eq!(strip_junk("Song Title (Official Video)"), "Song Title");
        assert_eq!(
            strip_junk("Song Title [Official Music Video]"),
            "Song Title"
        );
        assert_eq!(strip_junk("Song Title (Lyrics)"), "Song Title");
        assert_eq!(strip_junk("Song Title (HD)"), "Song Title");
    }

    #[test]
    fn test_strip_junk_keeps_clean_titles() {
        assert_eq!(strip_junk("Just A Song"), "Just A Song");
        assert_eq!(strip_junk("AC/DC - TNT"), "AC/DC - TNT");
    }

    #[test]
    fn test_split_artist_title() {
        assert_eq!(
            split_artist_title("Rammstein - Sonne"),
            Some(("Rammstein".to_string(), "Sonne".to_string()))
        );
        assert_eq!(split_artist_title("No Separator Here"), None);
        assert_eq!(split_artist_title(""), None);
    }

    #[test]
    fn test_track_meta_prefers_ytdlp_fields() {
        let info = json!({
            "title": "Artist - Song (Official Video)",
            "track": "Song",
            "artist": "Artist",
            "album": "Album",
        });
        let (artist, track, album) = track_meta(&info);
        assert_eq!(artist, "Artist");
        assert_eq!(track, "Song");
        assert_eq!(album, "Album");
    }

    #[test]
    fn test_track_meta_falls_back_to_title_split() {
        let info = json!({ "title": "Artist - Song" });
        let (artist, track, _) = track_meta(&info);
        assert_eq!(artist, "Artist");
        assert_eq!(track, "Song");
    }

    #[test]
    fn test_upscale_artwork_url() {
        assert_eq!(
            upscale_artwork_url("https://is1-ssl.mzstatic.com/img/100x100bb.jpg"),
            "https://is1-ssl.mzstatic.com/img/600x600bb.jpg"
        );
        assert_eq!(
            upscale_artwork_url("https://example.com/no-size-here.jpg"),
            "https://example.com/no-size-here.jpg"
        );
    }

    #[test]
    fn test_has_lrc_timestamps() {
        assert!(has_lrc_timestamps("[00:12.34] Hello"));
        assert!(has_lrc_timestamps("[1:2] Hello"));
        assert!(!has_lrc_timestamps("Hello world"));
        assert!(!has_lrc_timestamps(""));
    }

    #[test]
    fn test_strip_lrc_timestamps() {
        let synced = "[00:01.00]First line\n[00:05.50]Second line\n";
        assert_eq!(strip_lrc_timestamps(synced), "First line\nSecond line");
        assert_eq!(strip_lrc_timestamps("plain"), "plain");
    }

    #[test]
    fn test_lyrics_from_entry() {
        let entry = json!({"plainLyrics": "a", "syncedLyrics": "[00:01]a"});
        let l = lyrics_from_entry(&entry).unwrap();
        assert_eq!(l.plain.as_deref(), Some("a"));
        assert_eq!(l.synced.as_deref(), Some("[00:01]a"));

        assert!(lyrics_from_entry(&json!({"plainLyrics": "", "syncedLyrics": null})).is_none());
    }
}
