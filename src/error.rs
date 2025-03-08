/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/
use std::error::Error;
use std::fmt::{Display, Formatter};

/**
Errors that can occur when interacting with a DXLink device.

This enum represents various error conditions that might arise during communication with a DXLink device, including WebSocket errors, serialization issues, authentication failures, connection problems, channel issues, protocol violations, timeouts, unexpected messages, and unknown errors.
*/
#[derive(Debug)]
pub enum DXLinkError {
    /// Represents an error originating from the underlying WebSocket connection.
    WebSocket(tokio_tungstenite::tungstenite::Error),
    /// Represents an error that occurred during serialization or deserialization of JSON data.
    Serialization(serde_json::Error),
    /// Represents an authentication failure, such as invalid credentials.
    Authentication(String),
    /// Represents an error related to the connection, such as a failure to establish or maintain a connection.
    Connection(String),
    /// Represents an error specific to a channel, such as an invalid channel ID.
    Channel(String),
    /// Represents an error due to a violation of the DXLink protocol.
    Protocol(String),
    /// Represents a timeout error, such as a failure to receive a response within a specified time.
    Timeout(String),
    /// Represents the reception of an unexpected message from the DXLink device.
    UnexpectedMessage(String),
    /// Represents an unknown error that does not fit into any of the other categories.
    Unknown(String),
}

impl Display for DXLinkError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            DXLinkError::WebSocket(e) => write!(f, "WebSocket error: {}", e),
            DXLinkError::Serialization(e) => write!(f, "Serialization error: {}", e),
            DXLinkError::Authentication(e) => write!(f, "Authentication error: {}", e),
            DXLinkError::Connection(e) => write!(f, "Connection error: {}", e),
            DXLinkError::Channel(e) => write!(f, "Channel error: {}", e),
            DXLinkError::Protocol(e) => write!(f, "Protocol error: {}", e),
            DXLinkError::Timeout(e) => write!(f, "Timeout error: {}", e),
            DXLinkError::UnexpectedMessage(e) => write!(f, "Unexpected message: {}", e),
            DXLinkError::Unknown(e) => write!(f, "Unknown error: {}", e),
        }
    }
}

impl Error for DXLinkError {}

impl From<tokio_tungstenite::tungstenite::Error> for DXLinkError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        DXLinkError::WebSocket(e)
    }
}

impl From<serde_json::Error> for DXLinkError {
    fn from(e: serde_json::Error) -> Self {
        DXLinkError::Serialization(e)
    }
}

/// A type alias for `Result<T, DXLinkError>`.
///
/// This type is used as a return type for functions that can return a `DXLinkError`.
/// It represents the result of a DXLink operation, which can either be successful (returning a value of type `T`)
/// or an error (returning a `DXLinkError`).
///
/// # Example
///
/// ```rust
///  use dxlink::error::DXLinkResult;fn example_function() -> DXLinkResult<String> {
///      // ... some code that might return a DXLinkError ...
///  
///      Ok("Success!".to_string())
///  }
///  ```
pub type DXLinkResult<T> = Result<T, DXLinkError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as StdError;
    use tokio_tungstenite::tungstenite;

    // Test the Display implementation for DXLinkError
    #[test]
    fn test_error_display() {
        // Test WebSocket error display
        let ws_error = tungstenite::Error::ConnectionClosed;
        let error = DXLinkError::WebSocket(ws_error);
        assert!(format!("{}", error).starts_with("WebSocket error:"));

        // Test Serialization error display
        let ser_error = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let error = DXLinkError::Serialization(ser_error);
        assert!(format!("{}", error).starts_with("Serialization error:"));

        // Test string-based errors display
        let error = DXLinkError::Authentication("Invalid token".to_string());
        assert_eq!(format!("{}", error), "Authentication error: Invalid token");

        let error = DXLinkError::Connection("Connection refused".to_string());
        assert_eq!(format!("{}", error), "Connection error: Connection refused");

        let error = DXLinkError::Channel("Channel not found".to_string());
        assert_eq!(format!("{}", error), "Channel error: Channel not found");

        let error = DXLinkError::Protocol("Invalid protocol".to_string());
        assert_eq!(format!("{}", error), "Protocol error: Invalid protocol");

        let error = DXLinkError::Timeout("Operation timed out".to_string());
        assert_eq!(format!("{}", error), "Timeout error: Operation timed out");

        let error = DXLinkError::UnexpectedMessage("Unexpected message received".to_string());
        assert_eq!(
            format!("{}", error),
            "Unexpected message: Unexpected message received"
        );

        let error = DXLinkError::Unknown("Unknown error occurred".to_string());
        assert_eq!(
            format!("{}", error),
            "Unknown error: Unknown error occurred"
        );
    }

    // Test the Error trait implementation
    #[test]
    fn test_error_trait() {
        // Verify all DXLinkError types implement the Error trait
        fn assert_error<T: StdError>(_: T) {}

        let ws_error = tungstenite::Error::ConnectionClosed;
        assert_error(DXLinkError::WebSocket(ws_error));

        let ser_error = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        assert_error(DXLinkError::Serialization(ser_error));

        assert_error(DXLinkError::Authentication("test".to_string()));
        assert_error(DXLinkError::Connection("test".to_string()));
        assert_error(DXLinkError::Channel("test".to_string()));
        assert_error(DXLinkError::Protocol("test".to_string()));
        assert_error(DXLinkError::Timeout("test".to_string()));
        assert_error(DXLinkError::UnexpectedMessage("test".to_string()));
        assert_error(DXLinkError::Unknown("test".to_string()));
    }

    // Test the From implementations
    #[test]
    fn test_from_websocket_error() {
        let ws_error = tungstenite::Error::ConnectionClosed;
        let error: DXLinkError = ws_error.into();

        match error {
            DXLinkError::WebSocket(_) => {}
            _ => panic!("Expected WebSocket error"),
        }
    }

    #[test]
    fn test_from_serialization_error() {
        let ser_error = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let error: DXLinkError = ser_error.into();

        match error {
            DXLinkError::Serialization(_) => {}
            _ => panic!("Expected Serialization error"),
        }
    }

    // Test the DXLinkResult type alias
    #[test]
    fn test_result_type_alias() {
        let err_result: DXLinkResult<i32> = Err(DXLinkError::Unknown("test".to_string()));
        assert!(err_result.is_err());

        match err_result {
            Ok(_) => panic!("Expected error"),
            Err(e) => match e {
                DXLinkError::Unknown(msg) => assert_eq!(msg, "test"),
                _ => panic!("Expected Unknown error"),
            },
        }
    }

    // Test error conversion and propagation with ?
    #[test]
    fn test_error_propagation() {
        // Test function that returns a DXLinkResult
        fn returns_websocket_error() -> DXLinkResult<()> {
            let ws_error = tungstenite::Error::ConnectionClosed;
            Err(ws_error.into())
        }

        fn propagates_error() -> DXLinkResult<()> {
            returns_websocket_error()?;
            Ok(())
        }

        let result = propagates_error();
        assert!(result.is_err());
        match result {
            Ok(_) => panic!("Expected error"),
            Err(e) => match e {
                DXLinkError::WebSocket(_) => {}
                _ => panic!("Expected WebSocket error"),
            },
        }
    }
}
