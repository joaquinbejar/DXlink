/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use super::error::{DXLinkError, DXLinkResult};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_tungstenite::{WebSocketStream, connect_async};
use tracing::{debug, error};

pub struct WebSocketConnection {
    write: futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    read: futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}

impl WebSocketConnection {
    pub async fn connect(url: &str) -> DXLinkResult<Self> {
        debug!("Connecting to WebSocket at: {}", url);

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| DXLinkError::Connection(format!("Failed to connect: {}", e)))?;

        debug!("WebSocket connection established");

        let (write, read) = ws_stream.split();

        Ok(Self { write, read })
    }

    pub async fn send<T: Serialize>(&mut self, message: &T) -> DXLinkResult<()> {
        let json = serde_json::to_string(message)?;
        debug!("Sending message: {}", json);
        // Convertir String a Utf8Bytes explícitamente
        let utf8_bytes = Utf8Bytes::from(json);
        self.write.send(Message::Text(utf8_bytes)).await?;
        Ok(())
    }

    pub async fn receive(&mut self) -> DXLinkResult<String> {
        match self.read.next().await {
            Some(Ok(Message::Text(text))) => {
                // Convertir Utf8Bytes a String
                let text_string = text.to_string();
                debug!("Received message: {}", text_string);
                Ok(text_string)
            }
            Some(Ok(message)) => {
                debug!("Received non-text message: {:?}", message);
                Err(DXLinkError::UnexpectedMessage(
                    "Expected text message".to_string(),
                ))
            }
            Some(Err(e)) => {
                error!("WebSocket error: {}", e);
                Err(DXLinkError::WebSocket(e))
            }
            None => {
                error!("WebSocket connection closed unexpectedly");
                Err(DXLinkError::Connection(
                    "Connection closed unexpectedly".to_string(),
                ))
            }
        }
    }

    pub async fn receive_with_timeout(
        &mut self,
        duration: Duration,
    ) -> DXLinkResult<Option<String>> {
        match timeout(duration, self.read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                // Convertir Utf8Bytes a String
                let text_string = text.to_string();
                debug!("Received message: {}", text_string);
                Ok(Some(text_string))
            }
            Ok(Some(Ok(message))) => {
                debug!("Received non-text message: {:?}", message);
                Err(DXLinkError::UnexpectedMessage(
                    "Expected text message".to_string(),
                ))
            }
            Ok(Some(Err(e))) => {
                error!("WebSocket error: {}", e);
                Err(DXLinkError::WebSocket(e))
            }
            Ok(None) => {
                error!("WebSocket connection closed unexpectedly");
                Err(DXLinkError::Connection(
                    "Connection closed unexpectedly".to_string(),
                ))
            }
            Err(_) => {
                // Timeout
                Ok(None)
            }
        }
    }
}
