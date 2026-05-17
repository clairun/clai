//! API error types for Netdata Cloud communication.
//!
//! This module defines all possible errors that can occur when interacting
//! with the Netdata Cloud API. Using `thiserror` makes it easy to define
//! errors with automatic `Display` and `Error` trait implementations.
//!
//! # Rust Learning: Error Handling
//!
//! In Rust, we don't use exceptions. Instead, functions that can fail return
//! `Result<T, E>` where `T` is the success type and `E` is the error type.
//!
//! The `?` operator automatically propagates errors up the call stack:
//! ```ignore
//! fn fetch_data() -> Result<Data, ApiError> {
//!     let response = client.get(url).await?;  // Returns early if error
//!     Ok(response.json().await?)
//! }
//! ```
//!
//! # Error Serialization
//!
//! Errors are serialized following Netdata's API error format:
//! ```json
//! {
//!     "errorMsgKey": "ErrUnauthorized",
//!     "errorMessage": "Authentication failed - please login again",
//!     "errorCode": "unauthorized"
//! }
//! ```
//!
//! - `errorMsgKey`: Stable PascalCase key for exact error matching (e.g., "ErrQuotaExceeded")
//! - `errorMessage`: Human-readable message for display
//! - `errorCode`: General error category in snake_case (e.g., "forbidden")
//!
//! This allows JavaScript to match on exact conditions via `errorMsgKey`.

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error response from Netdata Cloud API.
///
/// When the API returns an error, we try to parse this structure
/// from the response body to get detailed error information.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetdataErrorResponse {
    /// Specific error key for exact matching (e.g., "SPACE_NOT_FOUND")
    #[serde(default)]
    pub error_msg_key: Option<String>,
    /// Human-readable error message
    #[serde(default)]
    pub error_message: Option<String>,
    /// Error code from the API
    #[serde(default)]
    #[allow(dead_code)]
    pub error_code: Option<String>,
}

/// Error codes for API errors.
///
/// These are stable identifiers that JavaScript can match on.
/// The codes are lowercase_snake_case for consistency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Network,
    Unauthorized,
    Forbidden,
    NotFound,
    Server,
    NoToken,
    TokenStorage,
    ParseError,
}

impl ErrorCode {
    /// Returns the string representation of the error code.
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::Network => "network",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Server => "server",
            ErrorCode::NoToken => "no_token",
            ErrorCode::TokenStorage => "token_storage",
            ErrorCode::ParseError => "parse_error",
        }
    }
}

/// Detailed error information for the frontend.
///
/// This struct holds all error details that will be serialized to JSON.
#[derive(Debug, Clone)]
pub struct ErrorDetails {
    /// Specific error key for exact matching (e.g., "INSUFFICIENT_CREDITS")
    pub error_msg_key: String,
    /// Human-readable error message
    pub error_message: String,
    /// General error category
    #[allow(dead_code)]
    pub error_code: ErrorCode,
}

/// Errors that can occur when calling the Netdata Cloud API.
///
/// # Rust Learning: Derive Macros
///
/// `#[derive(...)]` automatically implements traits for our struct/enum:
/// - `Error` - Makes this a proper Rust error type (from thiserror)
/// - `Debug` - Allows printing with `{:?}` for debugging
///
/// The `#[error("...")]` attribute defines the human-readable message
/// that's shown when the error is displayed (e.g., in logs).
#[derive(Error, Debug)]
pub enum ApiError {
    /// Network-level error (connection failed, timeout, etc.)
    ///
    /// `#[from]` automatically implements `From<reqwest::Error>` for ApiError,
    /// allowing the `?` operator to convert reqwest errors automatically.
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    /// HTTP 401 - Authentication failed (invalid or expired token)
    #[error("{}", .0.error_message)]
    Unauthorized(ErrorDetails),

    /// HTTP 403 - User lacks permission for this operation
    #[error("{}", .0.error_message)]
    Forbidden(ErrorDetails),

    /// HTTP 404 - Resource not found
    #[error("{}", .0.error_message)]
    NotFound(ErrorDetails),

    /// HTTP 5xx - Server-side error
    #[error("{}", .0.error_message)]
    Server(ErrorDetails),

    /// No authentication token is available
    #[error("No authentication token - please login")]
    NoToken,

    /// Error accessing the secure token storage
    #[error("Token storage error: {0}")]
    TokenStorage(String),

    /// JSON parsing error
    #[error("Failed to parse response: {0}")]
    ParseError(String),
}

impl ApiError {
    /// Returns the error code for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            ApiError::Network(_) => ErrorCode::Network,
            ApiError::Unauthorized(_) => ErrorCode::Unauthorized,
            ApiError::Forbidden(_) => ErrorCode::Forbidden,
            ApiError::NotFound(_) => ErrorCode::NotFound,
            ApiError::Server(_) => ErrorCode::Server,
            ApiError::NoToken => ErrorCode::NoToken,
            ApiError::TokenStorage(_) => ErrorCode::TokenStorage,
            ApiError::ParseError(_) => ErrorCode::ParseError,
        }
    }

    /// Returns the error message key for exact matching.
    /// Uses PascalCase following Netdata's convention (e.g., "ErrUnauthorized").
    pub fn error_msg_key(&self) -> &str {
        match self {
            ApiError::Network(_) => "ErrNetwork",
            ApiError::Unauthorized(details) => &details.error_msg_key,
            ApiError::Forbidden(details) => &details.error_msg_key,
            ApiError::NotFound(details) => &details.error_msg_key,
            ApiError::Server(details) => &details.error_msg_key,
            ApiError::NoToken => "ErrNoToken",
            ApiError::TokenStorage(_) => "ErrTokenStorage",
            ApiError::ParseError(_) => "ErrParseError",
        }
    }

    /// Creates an Unauthorized error with default message.
    #[allow(dead_code)]
    pub fn unauthorized() -> Self {
        ApiError::Unauthorized(ErrorDetails {
            error_msg_key: "ErrUnauthorized".to_string(),
            error_message: "Authentication failed - please login again".to_string(),
            error_code: ErrorCode::Unauthorized,
        })
    }

    /// Creates an error from a Netdata API error response.
    ///
    /// Default error keys follow Netdata's conventions:
    /// - ErrUnauthorized, ErrForbidden, ErrNotFound, ErrBadRequest
    /// - ErrUnprocessableEntity, ErrTooManyRequests
    /// - ErrInternal, ErrUnavailable, ErrServerOverloaded
    pub fn from_response(
        status: reqwest::StatusCode,
        netdata_error: Option<NetdataErrorResponse>,
    ) -> Self {
        // Default values following Netdata's error key conventions
        let (default_msg_key, default_message) = match status.as_u16() {
            400 => ("ErrBadRequest", "Bad request"),
            401 => ("ErrUnauthorized", "Authentication failed"),
            403 => ("ErrForbidden", "Access forbidden"),
            404 => ("ErrNotFound", "Resource not found"),
            422 => ("ErrUnprocessableEntity", "Unprocessable entity"),
            429 => ("ErrTooManyRequests", "Too many requests"),
            500 => ("ErrInternal", "Internal server error"),
            503 => ("ErrUnavailable", "Service unavailable"),
            _ if status.is_server_error() => ("ErrInternal", "Server error"),
            _ => ("ErrUnknown", "Unknown error"),
        };

        let details = ErrorDetails {
            error_msg_key: netdata_error
                .as_ref()
                .and_then(|e| e.error_msg_key.clone())
                .unwrap_or_else(|| default_msg_key.to_string()),
            error_message: netdata_error
                .as_ref()
                .and_then(|e| e.error_message.clone())
                .unwrap_or_else(|| default_message.to_string()),
            error_code: match status.as_u16() {
                401 => ErrorCode::Unauthorized,
                403 => ErrorCode::Forbidden,
                404 => ErrorCode::NotFound,
                _ => ErrorCode::Server,
            },
        };

        match status.as_u16() {
            401 => ApiError::Unauthorized(details),
            403 => ApiError::Forbidden(details),
            404 => ApiError::NotFound(details),
            _ => ApiError::Server(details),
        }
    }
}

/// Serialize ApiError following Netdata's error format.
///
/// # Output Format
///
/// ```json
/// {
///     "errorMsgKey": "ErrQuotaExceeded",
///     "errorMessage": "AI credits quota exceeded",
///     "errorCode": "forbidden"
/// }
/// ```
///
/// Note: `errorMsgKey` uses PascalCase following Netdata's convention.
///
/// # JavaScript Usage
///
/// ```javascript
/// try {
///     await invoke('api_get_data', { spaceId, roomId, query });
/// } catch (error) {
///     // Match exact conditions using PascalCase keys
///     if (error.errorMsgKey === 'ErrQuotaExceeded') {
///         showUpgradeDialog();
///     } else if (error.errorCode === 'unauthorized') {
///         promptForCredentials();
///     } else {
///         showError(error.errorMessage);
///     }
/// }
/// ```
impl Serialize for ApiError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("ApiError", 3)?;
        state.serialize_field("errorMsgKey", self.error_msg_key())?;
        state.serialize_field("errorMessage", &self.to_string())?;
        state.serialize_field("errorCode", self.code().as_str())?;
        state.end()
    }
}

/// Convenience type alias for Results with ApiError.
///
/// Instead of writing `Result<T, ApiError>` everywhere, we can write `ApiResult<T>`.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = ApiError::unauthorized();
        assert_eq!(
            err.to_string(),
            "Authentication failed - please login again"
        );
    }

    #[test]
    fn test_error_code() {
        assert_eq!(ApiError::unauthorized().code(), ErrorCode::Unauthorized);
        assert_eq!(ApiError::NoToken.code(), ErrorCode::NoToken);
        assert_eq!(
            ApiError::ParseError("test".to_string()).code(),
            ErrorCode::ParseError
        );
    }

    #[test]
    fn test_error_serialization() {
        let err = ApiError::from_response(reqwest::StatusCode::NOT_FOUND, None);
        let json = serde_json::to_string(&err).unwrap();

        // Should serialize as structured JSON matching Netdata format
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["errorCode"], "not_found");
        // When no Netdata error is provided, errorMsgKey defaults to PascalCase
        assert_eq!(parsed["errorMsgKey"], "ErrNotFound");
        assert_eq!(parsed["errorMessage"], "Resource not found");
    }

    #[test]
    fn test_unauthorized_serialization() {
        let err = ApiError::unauthorized();
        let json = serde_json::to_string(&err).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["errorCode"], "unauthorized");
        assert_eq!(parsed["errorMsgKey"], "ErrUnauthorized");
        assert_eq!(
            parsed["errorMessage"],
            "Authentication failed - please login again"
        );
    }

    #[test]
    fn test_from_response_with_netdata_error() {
        // Simulate a Netdata API error response (e.g., quota exceeded)
        let netdata_error = NetdataErrorResponse {
            error_msg_key: Some("ErrQuotaExceeded".to_string()),
            error_message: Some("AI credits quota exceeded".to_string()),
            error_code: Some("forbidden".to_string()),
        };

        let err = ApiError::from_response(reqwest::StatusCode::FORBIDDEN, Some(netdata_error));
        let json = serde_json::to_string(&err).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["errorCode"], "forbidden");
        assert_eq!(parsed["errorMsgKey"], "ErrQuotaExceeded");
        assert_eq!(parsed["errorMessage"], "AI credits quota exceeded");
    }
}
