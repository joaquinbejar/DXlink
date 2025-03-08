/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use super::error::{DXLinkError, DXLinkResult};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, connect_async};
use tracing::{debug, error};

pub struct WebSocketConnection {
    write: Arc<
        Mutex<futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>,
    >,
    read: Arc<Mutex<futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>>>,
}

impl WebSocketConnection {
    pub async fn connect(url: &str) -> DXLinkResult<Self> {
        debug!("Connecting to WebSocket at: {}", url);

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| DXLinkError::Connection(format!("Failed to connect: {}", e)))?;

        debug!("WebSocket connection established");

        let (write, read) = ws_stream.split();

        Ok(Self {
            write: Arc::new(Mutex::new(write)),
            read: Arc::new(Mutex::new(read)),
        })
    }

    pub async fn send<T: Serialize>(&self, message: &T) -> DXLinkResult<()> {
        let json = serde_json::to_string(message)?;
        debug!("Sending message: {}", json);

        let mut write = self.write.lock().await;
        write.send(Message::Text(json.into())).await?;
        Ok(())
    }

    pub async fn receive(&self) -> DXLinkResult<String> {
        let mut read = self.read.lock().await;

        match read.next().await {
            Some(Ok(Message::Text(text))) => {
                debug!("Received message: {}", text);
                Ok(text.to_string())
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

    pub async fn receive_with_timeout(&self, duration: Duration) -> DXLinkResult<Option<String>> {
        let read_future = self.receive();

        match timeout(duration, read_future).await {
            Ok(result) => result.map(Some),
            Err(_) => Ok(None), // Timeout
        }
    }

    // Método auxiliar para crear un nuevo keepalive sender
    pub fn create_keepalive_sender(&self) -> KeepAliveSender {
        KeepAliveSender {
            connection: self.clone(),
        }
    }
}

// Implementación de Clone que clona los Arc, no el contenido
impl Clone for WebSocketConnection {
    fn clone(&self) -> Self {
        Self {
            write: Arc::clone(&self.write),
            read: Arc::clone(&self.read),
        }
    }
}

// Estructuras auxiliares para keepalive
#[derive(Clone)]
pub struct KeepAliveSender {
    connection: WebSocketConnection,
}

impl KeepAliveSender {
    pub async fn send_keepalive(&self, channel: u32) -> DXLinkResult<()> {
        use crate::messages::KeepaliveMessage;

        let keepalive_msg = KeepaliveMessage {
            channel,
            message_type: "KEEPALIVE".to_string(),
        };

        self.connection.send(&keepalive_msg).await
    }
}
