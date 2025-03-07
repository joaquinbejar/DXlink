/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum DXLinkError {
    WebSocket(tokio_tungstenite::tungstenite::Error),
    Serialization(serde_json::Error),
    Authentication(String),
    Connection(String),
    Channel(String),
    Protocol(String),
    Timeout(String),
    UnexpectedMessage(String),
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

pub type DXLinkResult<T> = Result<T, DXLinkError>;
