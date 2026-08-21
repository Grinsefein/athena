//! Integration tests for error handling module

use athena::errors::{AppError, AppResult, ErrorResponse};
use axum::http::StatusCode;
use axum::response::IntoResponse;

#[test]
fn test_error_response_json_structure() {
    let err = AppError::DownloadNotFound {
        id: "nonexistent-123".to_string(),
    };
    let payload = err.to_response_payload();

    // Verify structure
    assert!(!payload.success);
    assert!(payload.error_code.is_some());
    assert_eq!(payload.error_code.unwrap(), "DOWNLOAD_NOT_FOUND");
}

#[test]
fn test_http_status_code_mapping() {
    // NOT_FOUND cases
    assert_eq!(
        AppError::DownloadNotFound {
            id: "x".to_string()
        }
        .status_code(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        AppError::FileNotFound {
            path: "/x".to_string()
        }
        .status_code(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        AppError::DownloadNotReady {
            status: "queued".to_string()
        }
        .status_code(),
        StatusCode::NOT_FOUND
    );

    // BAD_REQUEST cases
    assert_eq!(
        AppError::InvalidUrl {
            message: "bad".to_string()
        }
        .status_code(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        AppError::InvalidFormat {
            message: "bad".to_string()
        }
        .status_code(),
        StatusCode::BAD_REQUEST
    );

    // BAD_GATEWAY cases (external service issues)
    assert_eq!(
        AppError::ExternalCommand {
            message: "yt-dlp failed".to_string()
        }
        .status_code(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        AppError::ParseVideoInfo {
            message: "invalid json".to_string()
        }
        .status_code(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        AppError::DownloadFailed {
            message: "error".to_string()
        }
        .status_code(),
        StatusCode::BAD_GATEWAY
    );

    // INTERNAL_SERVER_ERROR cases
    assert_eq!(
        AppError::FileSystem {
            message: "io error".to_string()
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        AppError::ProcessWait {
            message: "wait failed".to_string()
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        AppError::Internal {
            message: "generic".to_string()
        }
        .status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );

    // SERVICE_UNAVAILABLE
    assert_eq!(
        AppError::ProcessSpawn {
            message: "spawn failed".to_string()
        }
        .status_code(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[test]
fn test_into_response_preserves_status() {
    let test_cases = vec![
        (
            AppError::DownloadNotFound {
                id: "x".to_string(),
            },
            StatusCode::NOT_FOUND,
        ),
        (
            AppError::InvalidUrl {
                message: "bad".to_string(),
            },
            StatusCode::BAD_REQUEST,
        ),
        (
            AppError::Internal {
                message: "oops".to_string(),
            },
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ];

    for (err, expected_status) in test_cases {
        let response = err.into_response();
        assert_eq!(response.status(), expected_status);
    }
}

#[test]
fn test_error_chaining_result_type() {
    fn operation_that_fails() -> AppResult<String> {
        Err(AppError::FileNotFound {
            path: "/tmp/missing.txt".to_string(),
        })
    }

    fn higher_level_operation() -> AppResult<String> {
        let result = operation_that_fails()?;
        Ok(result)
    }

    let result = higher_level_operation();
    assert!(result.is_err());

    match result {
        Err(AppError::FileNotFound { path }) => {
            assert_eq!(path, "/tmp/missing.txt");
        }
        _ => panic!("Expected FileNotFound error"),
    }
}

#[test]
fn test_error_display_formatting() {
    let err = AppError::ExternalCommand {
        message: "yt-dlp: command not found".to_string(),
    };
    let display = format!("{}", err);
    assert!(display.contains("yt-dlp: command not found"));
    assert!(display.contains("External command failed"));
}

#[test]
fn test_error_clone_preserves_data() {
    let original = AppError::DownloadFailed {
        message: "network timeout".to_string(),
    };
    let cloned = original.clone();

    assert_eq!(original.to_string(), cloned.to_string());
    assert_eq!(original.error_code(), cloned.error_code());
    assert_eq!(original.status_code(), cloned.status_code());
}

#[test]
fn test_error_debug_output() {
    let err = AppError::InvalidFormat {
        message: "unsupported codec".to_string(),
    };
    let debug = format!("{:?}", err);

    // Debug output should include variant name and fields
    assert!(debug.contains("InvalidFormat"));
    assert!(debug.contains("unsupported codec"));
}

#[test]
fn test_io_error_variants_conversion() {
    use std::io::{Error, ErrorKind};

    let io_errors = vec![
        Error::new(ErrorKind::NotFound, "file not found"),
        Error::new(ErrorKind::PermissionDenied, "no permission"),
        Error::new(ErrorKind::AlreadyExists, "exists"),
        Error::new(ErrorKind::WouldBlock, "blocking"),
        Error::new(ErrorKind::InvalidInput, "invalid"),
    ];

    for io_err in io_errors {
        let msg = io_err.to_string();
        let app_err: AppError = io_err.into();

        assert!(matches!(app_err, AppError::FileSystem { .. }));
        assert!(app_err.to_string().contains(&msg));
    }
}

#[test]
fn test_error_response_serde_roundtrip() {
    let response = ErrorResponse {
        success: false,
        error: "Something went wrong".to_string(),
        error_code: Some("ERR_001".to_string()),
    };

    let json = serde_json::to_string(&response).expect("serialization failed");
    let deserialized: ErrorResponse = serde_json::from_str(&json).expect("deserialization failed");

    assert_eq!(response, deserialized);
}

#[test]
fn test_error_response_skips_none_code() {
    let response = ErrorResponse {
        success: false,
        error: "Error".to_string(),
        error_code: None,
    };

    let json = serde_json::to_string(&response).expect("serialization failed");
    assert!(!json.contains("error_code"));
}

#[test]
fn test_app_result_ok_variant() {
    fn produce() -> AppResult<i32> {
        Ok(42)
    }
    let result = produce();
    assert_eq!(result.unwrap(), 42);
}

#[test]
fn test_app_result_error_variant() {
    fn produce() -> AppResult<i32> {
        Err(AppError::Internal {
            message: "computation failed".to_string(),
        })
    }

    let result = produce();
    assert!(result.is_err());
    let err = match result {
        Ok(_) => panic!("Expected error"),
        Err(e) => e,
    };
    assert!(matches!(err, AppError::Internal { .. }));
}

#[test]
fn test_error_message_field() {
    // Verify the message field is properly displayed
    let err = AppError::ParseVideoInfo {
        message: "invalid JSON at line 5".to_string(),
    };

    let display = err.to_string();
    assert!(display.contains("invalid JSON at line 5"));
}

#[test]
fn test_all_errors_are_send_sync() {
    // This test verifies at compile time that AppError is Send + Sync
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AppError>();
    assert_send_sync::<AppResult<String>>();
}

#[test]
fn test_error_with_special_characters() {
    let err = AppError::FileNotFound {
        path: "/path/with spaces/and-unicode-日本語.txt".to_string(),
    };
    let display = err.to_string();
    assert!(display.contains("日本語"));
    assert!(display.contains("/path/with spaces/and-unicode-"));
}

#[tokio::test]
async fn test_error_in_async_context() {
    async fn async_operation() -> AppResult<String> {
        Err(AppError::DownloadNotReady {
            status: "processing".to_string(),
        })
    }

    let result = async_operation().await;
    assert!(matches!(result, Err(AppError::DownloadNotReady { status }) if status == "processing"));
}

#[test]
fn test_error_ordering_by_code() {
    // Verify all error codes follow consistent naming pattern
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
        AppError::Internal {
            message: "x".to_string(),
        },
    ];

    for err in errors {
        let code = err.error_code();
        // All codes should be uppercase with underscores
        assert_eq!(code, code.to_ascii_uppercase());
        assert!(
            code.contains('_'),
            "Error code should contain underscores: {}",
            code
        );
        assert!(
            !code.contains(' '),
            "Error code should not contain spaces: {}",
            code
        );
    }
}
