use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Custom error type for the Athena application.
/// Provides rich error context and automatic HTTP response conversion.
#[derive(Error, Debug, Clone, PartialEq)]
pub enum AppError {
    /// Error when yt-dlp command fails or is not available
    #[error("External command failed: {message}")]
    ExternalCommand { message: String },

    /// Error parsing video metadata from yt-dlp output
    #[error("Failed to parse video info: {message}")]
    ParseVideoInfo { message: String },

    /// Error when the requested download is not found
    #[error("Download not found: {id}")]
    DownloadNotFound { id: String },

    /// Error when the requested file is not found on disk
    #[error("File not found: {path}")]
    FileNotFound { path: String },

    /// Error when download is still in progress or failed
    #[error("Download not ready: {status}")]
    DownloadNotReady { status: String },

    /// Error during file system operations
    #[error("File system error: {message}")]
    FileSystem { message: String },

    /// Error when spawning subprocess fails
    #[error("Process spawn failed: {message}")]
    ProcessSpawn { message: String },

    /// Error when waiting for subprocess to complete
    #[error("Process execution failed: {message}")]
    ProcessWait { message: String },

    /// Error when the download process itself fails (yt-dlp returned error)
    #[error("Download failed: {message}")]
    DownloadFailed { message: String },

    /// Error when invalid URL is provided
    #[error("Invalid URL: {message}")]
    InvalidUrl { message: String },

    /// Error when invalid format or quality is requested
    #[error("Invalid format or quality: {message}")]
    InvalidFormat { message: String },

    /// Error when request is not authenticated
    #[error("Unauthorized: {message}")]
    Unauthorized { message: String },

    /// Error when a client exceeds the per-IP rate limit
    #[error("Too many requests: {message}")]
    TooManyRequests { message: String },

    /// Generic internal server error (fallback)
    #[error("Internal server error: {message}")]
    Internal { message: String },
}

/// Error response payload sent to clients
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ErrorResponse {
    pub success: bool,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl AppError {
    /// Get the HTTP status code associated with this error
    pub fn status_code(&self) -> StatusCode {
        match self {
            AppError::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            AppError::TooManyRequests { .. } => StatusCode::TOO_MANY_REQUESTS,
            AppError::ExternalCommand { .. } => StatusCode::BAD_GATEWAY,
            AppError::ParseVideoInfo { .. } => StatusCode::BAD_GATEWAY,
            AppError::DownloadNotFound { .. } => StatusCode::NOT_FOUND,
            AppError::FileNotFound { .. } => StatusCode::NOT_FOUND,
            AppError::DownloadNotReady { .. } => StatusCode::NOT_FOUND,
            AppError::FileSystem { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::ProcessSpawn { .. } => StatusCode::SERVICE_UNAVAILABLE,
            AppError::ProcessWait { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::DownloadFailed { .. } => StatusCode::BAD_GATEWAY,
            AppError::InvalidUrl { .. } => StatusCode::BAD_REQUEST,
            AppError::InvalidFormat { .. } => StatusCode::BAD_REQUEST,
            AppError::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Get an error code string for programmatic error handling
    pub fn error_code(&self) -> &'static str {
        match self {
            AppError::Unauthorized { .. } => "UNAUTHORIZED",
            AppError::TooManyRequests { .. } => "RATE_LIMITED",
            AppError::ExternalCommand { .. } => "EXTERNAL_COMMAND_ERROR",
            AppError::ParseVideoInfo { .. } => "PARSE_ERROR",
            AppError::DownloadNotFound { .. } => "DOWNLOAD_NOT_FOUND",
            AppError::FileNotFound { .. } => "FILE_NOT_FOUND",
            AppError::DownloadNotReady { .. } => "DOWNLOAD_NOT_READY",
            AppError::FileSystem { .. } => "FILE_SYSTEM_ERROR",
            AppError::ProcessSpawn { .. } => "PROCESS_SPAWN_ERROR",
            AppError::ProcessWait { .. } => "PROCESS_WAIT_ERROR",
            AppError::DownloadFailed { .. } => "DOWNLOAD_FAILED",
            AppError::InvalidUrl { .. } => "INVALID_URL",
            AppError::InvalidFormat { .. } => "INVALID_FORMAT",
            AppError::Internal { .. } => "INTERNAL_ERROR",
        }
    }

    /// Create an error response payload
    pub fn to_response_payload(&self) -> ErrorResponse {
        ErrorResponse {
            success: false,
            error: self.to_string(),
            error_code: Some(self.error_code().to_string()),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = Json(self.to_response_payload());
        let mut response = (status, body).into_response();

        if matches!(self, AppError::TooManyRequests { .. }) {
            if let Ok(value) = HeaderValue::from_str(&RETRY_AFTER_SECS.to_string()) {
                response.headers_mut().insert("retry-after", value);
            }
        }

        response
    }
}

/// Seconds a client should wait before retrying after a 429 response.
const RETRY_AFTER_SECS: u64 = 60;

// Conversions from standard library errors
impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::FileSystem {
            message: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::ParseVideoInfo {
            message: err.to_string(),
        }
    }
}

impl From<std::string::FromUtf8Error> for AppError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        AppError::ParseVideoInfo {
            message: format!("Invalid UTF-8 in output: {}", err),
        }
    }
}

// Result type alias for convenience
pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_messages() {
        let err = AppError::DownloadNotFound {
            id: "abc123".to_string(),
        };
        assert_eq!(err.to_string(), "Download not found: abc123");

        let err = AppError::FileNotFound {
            path: "/tmp/file.txt".to_string(),
        };
        assert_eq!(err.to_string(), "File not found: /tmp/file.txt");
    }

    #[test]
    fn test_status_codes() {
        assert_eq!(
            AppError::DownloadNotFound {
                id: "test".to_string(),
            }
            .status_code(),
            StatusCode::NOT_FOUND
        );

        assert_eq!(
            AppError::InvalidUrl {
                message: "bad url".to_string(),
            }
            .status_code(),
            StatusCode::BAD_REQUEST
        );

        assert_eq!(
            AppError::ExternalCommand {
                message: "yt-dlp failed".to_string(),
            }
            .status_code(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn test_error_codes() {
        let err = AppError::DownloadFailed {
            message: "test".to_string(),
        };
        assert_eq!(err.error_code(), "DOWNLOAD_FAILED");

        let err = AppError::Internal {
            message: "test".to_string(),
        };
        assert_eq!(err.error_code(), "INTERNAL_ERROR");
    }

    #[test]
    fn test_response_payload() {
        let err = AppError::InvalidFormat {
            message: "unsupported format".to_string(),
        };
        let payload = err.to_response_payload();

        assert!(!payload.success);
        assert_eq!(
            payload.error,
            "Invalid format or quality: unsupported format"
        );
        assert_eq!(payload.error_code, Some("INVALID_FORMAT".to_string()));
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let app_err: AppError = io_err.into();

        assert!(matches!(app_err, AppError::FileSystem { .. }));
        assert!(app_err.to_string().contains("file missing"));
    }

    #[test]
    fn test_json_error_conversion() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let app_err: AppError = json_err.into();

        assert!(matches!(app_err, AppError::ParseVideoInfo { .. }));
    }

    #[test]
    fn test_utf8_error_conversion() {
        let bytes = vec![0x80, 0x81, 0x82];
        let utf8_err = String::from_utf8(bytes).unwrap_err();
        let app_err: AppError = utf8_err.into();

        assert!(matches!(app_err, AppError::ParseVideoInfo { .. }));
    }

    #[test]
    fn test_into_response() {
        let err = AppError::DownloadNotFound {
            id: "test123".to_string(),
        };
        let response = err.into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_all_error_variants_have_unique_codes() {
        let errors = vec![
            AppError::ExternalCommand {
                message: "x".to_string(),
            },
            AppError::ParseVideoInfo {
                message: "x".to_string(),
            },
            AppError::DownloadNotFound {
                id: "x".to_string(),
            },
            AppError::FileNotFound {
                path: "x".to_string(),
            },
            AppError::DownloadNotReady {
                status: "x".to_string(),
            },
            AppError::FileSystem {
                message: "x".to_string(),
            },
            AppError::ProcessSpawn {
                message: "x".to_string(),
            },
            AppError::ProcessWait {
                message: "x".to_string(),
            },
            AppError::DownloadFailed {
                message: "x".to_string(),
            },
            AppError::InvalidUrl {
                message: "x".to_string(),
            },
            AppError::InvalidFormat {
                message: "x".to_string(),
            },
            AppError::Unauthorized {
                message: "x".to_string(),
            },
            AppError::TooManyRequests {
                message: "x".to_string(),
            },
            AppError::Internal {
                message: "x".to_string(),
            },
        ];

        let codes: Vec<_> = errors.iter().map(|e| e.error_code()).collect();
        let unique_codes: std::collections::HashSet<_> = codes.iter().cloned().collect();

        assert_eq!(
            codes.len(),
            unique_codes.len(),
            "All error codes should be unique"
        );
    }

    #[test]
    fn test_error_partial_eq() {
        let err1 = AppError::DownloadNotFound {
            id: "abc".to_string(),
        };
        let err2 = AppError::DownloadNotFound {
            id: "abc".to_string(),
        };
        let err3 = AppError::DownloadNotFound {
            id: "def".to_string(),
        };

        assert_eq!(err1, err2);
        assert_ne!(err1, err3);
    }

    #[test]
    fn test_generate_icon() {
        let size = 512;
        let mut img = image::ImageBuffer::new(size, size);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let px = x as f32;
            let py = y as f32;

            let r = 120.0;
            let pad = 20.0;
            let left = pad + r;
            let right = size as f32 - pad - r;
            let top = pad + r;
            let bottom = size as f32 - pad - r;

            let inside = if px < pad || px > size as f32 - pad || py < pad || py > size as f32 - pad
            {
                false
            } else if px < left && py < top {
                (px - left).powi(2) + (py - top).powi(2) <= r.powi(2)
            } else if px > right && py < top {
                (px - right).powi(2) + (py - top).powi(2) <= r.powi(2)
            } else if px < left && py > bottom {
                (px - left).powi(2) + (py - bottom).powi(2) <= r.powi(2)
            } else if px > right && py > bottom {
                (px - right).powi(2) + (py - bottom).powi(2) <= r.powi(2)
            } else {
                true
            };

            if inside {
                let v1 = (333.0, 153.0);
                let v2 = (333.0, 358.0);
                let v3 = (128.0, 256.0);

                let sign = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| {
                    (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
                };

                let d1 = sign((px, py), v1, v2);
                let d2 = sign((px, py), v2, v3);
                let d3 = sign((px, py), v3, v1);

                let has_neg = (d1 < 0.0) || (d2 < 0.0) || (d3 < 0.0);
                let has_pos = (d1 > 0.0) || (d2 > 0.0) || (d3 > 0.0);

                let inside_triangle = !(has_neg && has_pos);

                if inside_triangle {
                    *pixel = image::Rgb([255u8, 255u8, 255u8]);
                } else {
                    *pixel = image::Rgb([67u8, 78u8, 209u8]);
                }
            } else {
                *pixel = image::Rgb([15u8, 23u8, 42u8]);
            }
        }
        img.save("app-icon.png").unwrap();
    }
}
