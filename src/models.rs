use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "default_quality")]
    pub quality: String,
    #[serde(default)]
    pub playlist_urls: Option<Vec<String>>,
    #[serde(default)]
    pub lyrics: bool,
}

fn default_format() -> String {
    String::from("mp4")
}
fn default_quality() -> String {
    String::from("best")
}

#[derive(Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PlaylistVideo {
    pub id: String,
    pub title: String,
    pub url: String,
}

#[derive(Serialize)]
pub struct AnalyzeResponse {
    pub id: String,
    pub url: String,
    pub title: String,
    pub description: String,
    pub thumbnail: String,
    pub duration: String,
    pub author: String,
    pub formats: Vec<FormatInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_videos: Option<Vec<PlaylistVideo>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FormatInfo {
    pub media_type: String,
    pub format: String,
    pub quality: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesize: Option<String>,
}

#[derive(Serialize)]
pub struct DownloadResponse {
    pub download_id: String,
    pub status: String,
}

#[derive(Serialize, Clone)]
pub struct ProgressUpdate {
    pub status: String,
    pub progress: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lyrics_plain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lyrics_synced: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
}

#[derive(Serialize)]
pub struct ConfigResponse {
    pub auth_enabled: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ShareRequest {
    pub url: Option<String>,
    pub text: Option<String>,
    pub title: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_format() {
        assert_eq!(default_format(), "mp4");
    }

    #[test]
    fn test_default_quality() {
        assert_eq!(default_quality(), "best");
    }

    #[test]
    fn test_api_response_serialization() {
        let response: ApiResponse<String> = ApiResponse {
            success: true,
            data: Some("test data".to_string()),
            error: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"data\":\"test data\""));
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_api_response_error() {
        let response: ApiResponse<String> = ApiResponse {
            success: false,
            data: None,
            error: Some("error message".to_string()),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"error\":\"error message\""));
        assert!(!json.contains("data"));
    }

    #[test]
    fn test_format_info_serialization() {
        let info = FormatInfo {
            media_type: "video".to_string(),
            format: "mp4".to_string(),
            quality: "1080p".to_string(),
            label: "1080p (mp4)".to_string(),
            filesize: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"media_type\":\"video\""));
        assert!(json.contains("\"format\":\"mp4\""));
        assert!(json.contains("\"quality\":\"1080p\""));
        assert!(json.contains("\"label\":\"1080p"));
    }

    #[test]
    fn test_progress_update_serialization() {
        let update = ProgressUpdate {
            status: "completed".to_string(),
            progress: 100.0,
            download_url: Some("/api/file/123".to_string()),
            error: None,
            speed: Some("5.67 MiB/s".to_string()),
            eta: Some("00:15".to_string()),
            lyrics_plain: Some("First line".to_string()),
            lyrics_synced: Some("[00:01.00]First line".to_string()),
        };
        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("\"status\":\"completed\""));
        assert!(json.contains("\"progress\":100.0"));
        assert!(json.contains("\"download_url\":\"/api/file/123\""));
        assert!(json.contains("\"speed\":\"5.67 MiB/s\""));
        assert!(json.contains("\"eta\":\"00:15\""));
        assert!(json.contains("\"lyrics_plain\":\"First line\""));
        assert!(json.contains("\"lyrics_synced\":\"[00:01.00]First line\""));
    }

    #[test]
    fn test_progress_update_skips_null() {
        let update = ProgressUpdate {
            status: "processing".to_string(),
            progress: 50.0,
            download_url: None,
            error: None,
            speed: None,
            eta: None,
            lyrics_plain: None,
            lyrics_synced: None,
        };
        let json = serde_json::to_string(&update).unwrap();
        assert!(!json.contains("download_url"));
        assert!(!json.contains("error"));
        assert!(!json.contains("\"speed\""));
        assert!(!json.contains("\"eta\""));
        assert!(!json.contains("lyrics"));
    }

    #[test]
    fn test_download_request_deserialization() {
        let json = r#"{"url":"https://youtube.com/watch?v=test","format":"mp4","quality":"1080p"}"#;
        let request: DownloadRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.url, "https://youtube.com/watch?v=test");
        assert_eq!(request.format, "mp4");
        assert_eq!(request.quality, "1080p");
    }

    #[test]
    fn test_download_request_defaults() {
        let json = r#"{"url":"https://youtube.com/watch?v=test"}"#;
        let request: DownloadRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.url, "https://youtube.com/watch?v=test");
        assert_eq!(request.format, "mp4");
        assert_eq!(request.quality, "best");
    }
}
