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

    pub fn create_keepalive_sender(&self) -> KeepAliveSender {
        KeepAliveSender {
            connection: self.clone(),
        }
    }
}

impl Clone for WebSocketConnection {
    fn clone(&self) -> Self {
        Self {
            write: Arc::clone(&self.write),
            read: Arc::clone(&self.read),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use warp::Filter;
    use warp::ws::{Message as WarpMessage, WebSocket as WarpWebSocket};

    async fn setup_test_server() -> (SocketAddr, mpsc::Receiver<String>, mpsc::Sender<String>) {
        let (client_tx, client_rx) = mpsc::channel::<String>(10);
        let (server_tx, server_rx) = mpsc::channel::<String>(10);

        let client_tx = Arc::new(Mutex::new(client_tx));
        let server_rx = Arc::new(Mutex::new(server_rx));

        let websocket = warp::path("websocket")
            .and(warp::ws())
            .map(move |ws: warp::ws::Ws| {
                let client_tx = client_tx.clone();
                let server_rx = server_rx.clone();

                ws.on_upgrade(move |websocket| handle_websocket(websocket, client_tx, server_rx))
            });

        let (addr, server) = warp::serve(websocket).bind_ephemeral(([127, 0, 0, 1], 0));

        tokio::spawn(server);

        (addr, client_rx, server_tx)
    }

    async fn handle_websocket(
        websocket: WarpWebSocket,
        client_tx: Arc<Mutex<mpsc::Sender<String>>>,
        server_rx: Arc<Mutex<mpsc::Receiver<String>>>,
    ) {
        let (mut ws_tx, mut ws_rx) = websocket.split();

        let server_to_client = tokio::spawn(async move {
            let mut rx = server_rx.lock().await;
            while let Some(msg) = rx.recv().await {
                ws_tx
                    .send(WarpMessage::text(msg))
                    .await
                    .expect("Failed to send message");
            }
        });

        let client_to_server = tokio::spawn(async move {
            let tx = client_tx.lock().await;
            while let Some(result) = ws_rx.next().await {
                match result {
                    Ok(msg) if msg.is_text() => {
                        if let Ok(text) = msg.to_str() {
                            tx.send(text.to_string())
                                .await
                                .expect("Failed to send to channel");
                        }
                    }
                    _ => break,
                }
            }
        });

        let _ = tokio::join!(server_to_client, client_to_server);
    }

    #[tokio::test]
    async fn test_websocket_connection() {
        // Configurar servidor de prueba
        let (addr, mut client_rx, server_tx) = setup_test_server().await;

        // Crear URL de conexión
        let ws_url = format!("ws://{}/websocket", addr);

        // Crear una conexión real
        let connection = WebSocketConnection::connect(&ws_url)
            .await
            .expect("Failed to connect");

        // Crear y enviar un mensaje de prueba
        #[derive(Serialize)]
        struct TestMessage {
            channel: u32,
            #[serde(rename = "type")]
            message_type: String,
            data: String,
        }

        let test_msg = TestMessage {
            channel: 1,
            message_type: "TEST".to_string(),
            data: "Hello, World!".to_string(),
        };

        // Enviar el mensaje
        connection
            .send(&test_msg)
            .await
            .expect("Failed to send message");

        // Verificar que el mensaje fue recibido por el servidor
        if let Some(received) = client_rx.recv().await {
            let parsed: serde_json::Value = serde_json::from_str(&received).unwrap();
            assert_eq!(parsed["channel"], 1);
            assert_eq!(parsed["type"], "TEST");
            assert_eq!(parsed["data"], "Hello, World!");
        } else {
            panic!("No message received");
        }

        server_tx
            .send("test_response".to_string())
            .await
            .expect("Failed to send from server");

        let received = connection
            .receive()
            .await
            .expect("Failed to receive message");
        assert_eq!(received, "test_response");
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::sleep;
    use warp::Filter;
    use warp::ws::{Message as WarpMessage, WebSocket as WarpWebSocket};

    async fn setup_test_server() -> (
        SocketAddr,
        mpsc::Receiver<String>,
        mpsc::Sender<String>,
        mpsc::Sender<bool>,
    ) {
        // Channels for communication with test server
        let (client_tx, client_rx) = mpsc::channel::<String>(10);
        let (server_tx, server_rx) = mpsc::channel::<String>(10);
        let (binary_tx, binary_rx) = mpsc::channel::<bool>(10);

        let client_tx = Arc::new(tokio::sync::Mutex::new(client_tx));
        let server_rx = Arc::new(tokio::sync::Mutex::new(server_rx));
        let binary_rx = Arc::new(tokio::sync::Mutex::new(binary_rx));

        // Define WebSocket test endpoint
        let websocket = warp::path("websocket")
            .and(warp::ws())
            .map(move |ws: warp::ws::Ws| {
                let client_tx = client_tx.clone();
                let server_rx = server_rx.clone();
                let binary_rx = binary_rx.clone();

                ws.on_upgrade(move |websocket| {
                    handle_websocket(websocket, client_tx, server_rx, binary_rx)
                })
            });

        // Start server on random port
        let (addr, server) = warp::serve(websocket).bind_ephemeral(([127, 0, 0, 1], 0));

        // Run server in separate tokio task
        tokio::spawn(server);

        (addr, client_rx, server_tx, binary_tx)
    }

    async fn handle_websocket(
        websocket: WarpWebSocket,
        client_tx: Arc<tokio::sync::Mutex<mpsc::Sender<String>>>,
        server_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<String>>>,
        binary_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<bool>>>,
    ) {
        let (ws_tx, mut ws_rx) = websocket.split();

        // Wrap ws_tx in Arc<Mutex<>> so it can be shared between tasks
        let ws_tx = Arc::new(tokio::sync::Mutex::new(ws_tx));

        // Task to send text messages to client
        let ws_tx_clone = ws_tx.clone();
        let server_to_client = tokio::spawn(async move {
            let mut rx = server_rx.lock().await;
            while let Some(msg) = rx.recv().await {
                let mut tx = ws_tx_clone.lock().await;
                tx.send(WarpMessage::text(msg))
                    .await
                    .expect("Failed to send message");
            }
        });

        // Task to send binary messages to client
        let binary_to_client = tokio::spawn(async move {
            let mut rx = binary_rx.lock().await;
            while let Some(_) = rx.recv().await {
                // Send a binary message
                let mut tx = ws_tx.lock().await;
                tx.send(WarpMessage::binary(vec![1, 2, 3]))
                    .await
                    .expect("Failed to send binary message");
            }
        });

        // Task to receive messages from client
        let client_to_server = tokio::spawn(async move {
            let tx = client_tx.lock().await;
            while let Some(result) = ws_rx.next().await {
                match result {
                    Ok(msg) if msg.is_text() => {
                        if let Ok(text) = msg.to_str() {
                            tx.send(text.to_string())
                                .await
                                .expect("Failed to send to channel");
                        }
                    }
                    _ => break,
                }
            }
        });

        // Wait for all tasks to complete
        let _ = tokio::join!(server_to_client, binary_to_client, client_to_server);
    }

    // Test for receive_with_timeout with successful response
    #[tokio::test]
    async fn test_receive_with_timeout_success() {
        // Set up test server
        let (addr, _client_rx, server_tx, _binary_tx) = setup_test_server().await;

        // Create connection URL
        let ws_url = format!("ws://{}/websocket", addr);

        // Create a real connection
        let connection = WebSocketConnection::connect(&ws_url)
            .await
            .expect("Failed to connect");

        // Send a message from server after a short delay
        let server_tx_clone = server_tx.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            server_tx_clone
                .send("test_response".to_string())
                .await
                .expect("Failed to send from server");
        });

        // Test receive_with_timeout with successful response
        let result = connection
            .receive_with_timeout(Duration::from_millis(500))
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("test_response".to_string()));
    }

    // Test for receive_with_timeout with timeout
    #[tokio::test]
    async fn test_receive_with_timeout_timeout() {
        // Set up test server
        let (addr, _client_rx, _server_tx, _binary_tx) = setup_test_server().await;

        // Create connection URL
        let ws_url = format!("ws://{}/websocket", addr);

        // Create a real connection
        let connection = WebSocketConnection::connect(&ws_url)
            .await
            .expect("Failed to connect");

        // Test receive_with_timeout with timeout
        let result = connection
            .receive_with_timeout(Duration::from_millis(100))
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    // Test receiving a non-text message
    #[tokio::test]
    async fn test_receive_non_text_message() {
        // Set up test server
        let (addr, _client_rx, _server_tx, binary_tx) = setup_test_server().await;

        // Create connection URL
        let ws_url = format!("ws://{}/websocket", addr);

        // Create a real connection
        let connection = WebSocketConnection::connect(&ws_url)
            .await
            .expect("Failed to connect");

        // Trigger server to send a binary message
        binary_tx
            .send(true)
            .await
            .expect("Failed to trigger binary message");

        // Try to receive the binary message
        let result = connection.receive().await;

        // Should result in an UnexpectedMessage error
        assert!(result.is_err());
        match result {
            Err(DXLinkError::UnexpectedMessage(msg)) => {
                assert!(msg.contains("Expected text message"));
            }
            _ => panic!("Expected UnexpectedMessage error, got: {:?}", result),
        }
    }

    // Test the clone implementation
    #[tokio::test]
    async fn test_clone() {
        // Set up test server
        let (addr, _client_rx, server_tx, _binary_tx) = setup_test_server().await;

        // Create connection URL
        let ws_url = format!("ws://{}/websocket", addr);

        // Create a real connection
        let connection = WebSocketConnection::connect(&ws_url)
            .await
            .expect("Failed to connect");

        // Clone the connection
        let connection_clone = connection.clone();

        // Send a message from server
        server_tx
            .send("test_message".to_string())
            .await
            .expect("Failed to send from server");

        // Both connections should be able to receive the message
        let result = connection.receive().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test_message");

        // Send another message for the clone
        server_tx
            .send("clone_message".to_string())
            .await
            .expect("Failed to send from server");

        // The clone should receive the message
        let result = connection_clone.receive().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "clone_message");
    }

    // Test the KeepAliveSender with the cloned connection
    #[tokio::test]
    async fn test_keepalive_sender_with_clone() {
        // Set up test server
        let (addr, mut client_rx, _server_tx, _binary_tx) = setup_test_server().await;

        // Create connection URL
        let ws_url = format!("ws://{}/websocket", addr);

        // Create a real connection
        let connection = WebSocketConnection::connect(&ws_url)
            .await
            .expect("Failed to connect");

        // Create a KeepAliveSender from the connection
        let keepalive_sender = connection.create_keepalive_sender();

        // Send a keepalive message
        keepalive_sender
            .send_keepalive(5)
            .await
            .expect("Failed to send keepalive");

        // Verify that the keepalive was sent
        if let Some(received) = client_rx.recv().await {
            let parsed: serde_json::Value = serde_json::from_str(&received).unwrap();
            assert_eq!(parsed["channel"], 5);
            assert_eq!(parsed["type"], "KEEPALIVE");
        } else {
            panic!("No keepalive message received");
        }

        // Clone the connection and create another KeepAliveSender
        let connection_clone = connection.clone();
        let keepalive_sender2 = connection_clone.create_keepalive_sender();

        // Send another keepalive message
        keepalive_sender2
            .send_keepalive(10)
            .await
            .expect("Failed to send keepalive from clone");

        // Verify that the second keepalive was sent
        if let Some(received) = client_rx.recv().await {
            let parsed: serde_json::Value = serde_json::from_str(&received).unwrap();
            assert_eq!(parsed["channel"], 10);
            assert_eq!(parsed["type"], "KEEPALIVE");
        } else {
            panic!("No keepalive message received from clone");
        }
    }
}
