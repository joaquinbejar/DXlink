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

/// Placeholder written in place of credential values when logging outbound messages.
const REDACTED_PLACEHOLDER: &str = "<redacted>";

/// Returns a copy of a serialized message that is safe to log.
///
/// Credential-bearing fields (any field named `token`, at any nesting level)
/// are replaced with [`REDACTED_PLACEHOLDER`] so that secrets such as the
/// DXLink auth token never reach the logs. If the payload cannot be parsed
/// back as JSON, the whole payload is withheld from the log rather than
/// risking a credential leak.
fn redact_sensitive(json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(mut value) => {
            redact_value(&mut value);
            value.to_string()
        }
        Err(_) => REDACTED_PLACEHOLDER.to_string(),
    }
}

/// Recursively masks the values of credential-bearing fields in a JSON value.
fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, field) in map.iter_mut() {
                if key.eq_ignore_ascii_case("token") {
                    *field = serde_json::Value::String(REDACTED_PLACEHOLDER.to_string());
                } else {
                    redact_value(field);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_value(item);
            }
        }
        _ => {}
    }
}

/// Represents a WebSocket connection.
///
/// This struct holds the read and write components of a WebSocket connection,
/// allowing for bidirectional communication.  It uses Arc and Mutex to enable
/// shared, thread-safe access to the underlying streams.
///
/// # Fields
///
/// * `write`:  An `Arc<Mutex>` wrapping the write sink of the WebSocket.  This allows
///   sending messages over the connection.  The sink is of type
///   `futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>`,
///   meaning it accepts `Message` objects and writes them to a potentially TLS-secured
///   TCP stream wrapped in a WebSocket.
///
/// * `read`: An `Arc<Mutex>` wrapping the read stream of the WebSocket.  This allows
///   receiving messages from the connection.  The stream is of type
///   `futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>`,
///   meaning it yields `Message` objects read from a potentially TLS-secured
///   TCP stream wrapped in a WebSocket.
///
#[derive(Debug)]
pub struct WebSocketConnection {
    write: Arc<
        Mutex<futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>,
    >,
    read: Arc<Mutex<futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>>>,
}

impl WebSocketConnection {
    /// Establishes a WebSocket connection to the specified URL.
    ///
    /// This function attempts to create a new WebSocket connection to the provided URL.  It uses
    /// `tokio_tungstenite` to handle the connection process. Upon successful connection, it splits
    /// the stream into read and write components, wrapping them in `Arc<Mutex>` for thread-safe
    /// shared access.  If any error occurs during the connection process, a `DXLinkError::Connection`
    /// error is returned.
    ///
    /// # Arguments
    ///
    /// * `url`: A string slice representing the URL of the WebSocket server.
    ///
    /// # Returns
    ///
    /// A `DXLinkResult` containing a `WebSocketConnection` if the connection is successful, or a
    /// `DXLinkError` if an error occurs.
    ///
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

    /// Sends a serialized message over the WebSocket connection.
    ///
    /// This function serializes the given message into a JSON string and sends it over the WebSocket connection.
    /// It acquires a lock on the write portion of the connection before sending the message.
    ///
    /// The debug log emitted by this function redacts credential-bearing fields
    /// (such as the `token` of an auth message), so enabling `debug`-level
    /// logging never leaks secrets.
    ///
    /// # Arguments
    ///
    /// * `message` - A reference to the message to be sent.  The message must implement the `Serialize` trait from the `serde` crate.
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the message was successfully sent.
    /// * `Err(DXLinkError)` if an error occurred during serialization or sending.
    ///
    pub async fn send<T: Serialize>(&self, message: &T) -> DXLinkResult<()> {
        let json = serde_json::to_string(message)?;
        debug!("Sending message: {}", redact_sensitive(&json));

        let mut write = self.write.lock().await;
        write.send(Message::Text(json.into())).await?;
        Ok(())
    }

    /// Receives a text message from the WebSocket connection.
    ///
    /// DXLink is a text-JSON protocol, so only `Text` frames carry payloads.
    /// WebSocket control frames (`Ping`, `Pong`) are ordinary transport traffic —
    /// the underlying library answers Pings on its own but still surfaces them
    /// here — so they are skipped and reading continues until a payload arrives.
    ///
    /// A `Close` frame is reported as [`DXLinkError::Connection`] rather than as
    /// an unexpected message: a server that closes is usually saying something
    /// specific (bad token, unsupported version) and that belongs in the error
    /// text. A `Binary` frame is a genuine protocol anomaly for DXLink and is
    /// rejected as [`DXLinkError::UnexpectedMessage`].
    ///
    /// # Returns
    ///
    /// * `Ok(String)`:  A string containing the received text message if successful.
    /// * `Err(DXLinkError)`:  A `DXLinkError` indicating the type of error encountered.
    ///   This could be a WebSocket error, an unexpected message type, or a connection error.
    ///
    pub async fn receive(&self) -> DXLinkResult<String> {
        let mut read = self.read.lock().await;

        loop {
            match read.next().await {
                Some(Ok(Message::Text(text))) => {
                    debug!("Received message: {}", text);
                    return Ok(text.to_string());
                }
                // Control frames are normal protocol traffic, not a protocol error.
                // `Frame` is documented by tungstenite as never yielded while
                // reading; the arm is defensive so a future change cannot make it
                // fatal by accident.
                Some(Ok(message @ (Message::Ping(_) | Message::Pong(_) | Message::Frame(_)))) => {
                    debug!("Skipping WebSocket control frame: {:?}", message);
                }
                Some(Ok(Message::Close(frame))) => {
                    let detail = match frame {
                        Some(frame) => format!("code {}, reason: {}", frame.code, frame.reason),
                        None => "no close frame".to_string(),
                    };
                    error!("Server closed the connection: {}", detail);
                    return Err(DXLinkError::Connection(format!(
                        "server closed the connection ({})",
                        detail
                    )));
                }
                Some(Ok(Message::Binary(bytes))) => {
                    debug!("Received binary frame of {} bytes", bytes.len());
                    return Err(DXLinkError::UnexpectedMessage(format!(
                        "Expected text message, got a binary frame of {} bytes",
                        bytes.len()
                    )));
                }
                Some(Err(e)) => {
                    error!("WebSocket error: {}", e);
                    return Err(DXLinkError::WebSocket(Box::new(e)));
                }
                None => {
                    error!("WebSocket connection closed unexpectedly");
                    return Err(DXLinkError::Connection(
                        "Connection closed unexpectedly".to_string(),
                    ));
                }
            }
        }
    }

    /// Receives a text message from the WebSocket connection with a timeout.
    ///
    /// This function attempts to read the next message from the WebSocket stream within the specified duration.
    /// It behaves like [`receive`](WebSocketConnection::receive), but returns `Ok(None)` if the timeout is reached before a message is received.
    ///
    /// # Arguments
    ///
    /// * `duration`: The maximum time to wait for a message.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(String))`: A string containing the received text message if successful.
    /// * `Ok(None)`: If the timeout is reached before a message is received.
    /// * `Err(DXLinkError)`: A `DXLinkError` indicating the type of error encountered.  This could be a WebSocket error, an unexpected message type, or a connection error.
    ///
    pub async fn receive_with_timeout(&self, duration: Duration) -> DXLinkResult<Option<String>> {
        let read_future = self.receive();

        match timeout(duration, read_future).await {
            Ok(result) => result.map(Some),
            Err(_) => Ok(None), // Timeout
        }
    }

    /// Creates a new `KeepAliveSender` instance.
    ///
    /// This function returns a `KeepAliveSender` that can be used to send
    /// keep-alive messages over the WebSocket connection.  The returned sender
    /// is a clone of the underlying connection, allowing multiple parts of the
    /// application to share the responsibility of sending keep-alives without
    /// needing to manage the underlying connection directly.
    ///
    /// # Returns
    ///
    /// A new `KeepAliveSender` instance.
    pub fn create_keepalive_sender(&self) -> KeepAliveSender {
        KeepAliveSender {
            connection: self.clone(),
        }
    }
}

/// Implements the `Clone` trait for `WebSocketConnection`.
///
/// This allows creating a new `WebSocketConnection` instance that shares the underlying
/// read and write streams with the original connection.  The cloning process uses
/// `Arc::clone` to increment the reference count of the shared `Arc` pointers, ensuring
/// that the underlying streams are not closed until all cloned instances are dropped.
///
/// This is useful for sharing a single WebSocket connection across multiple parts
/// of an application without needing to establish multiple separate connections.
impl Clone for WebSocketConnection {
    fn clone(&self) -> Self {
        Self {
            write: Arc::clone(&self.write),
            read: Arc::clone(&self.read),
        }
    }
}

/**
Sends keep-alive messages over a WebSocket connection.

This struct holds a `WebSocketConnection` and is used to send keep-alive messages
to maintain the connection.  It is cloneable to allow multiple parts of the
application to share the responsibility of sending keep-alives.
*/
#[derive(Clone)]
pub struct KeepAliveSender {
    /// The underlying WebSocket connection used for sending keep-alive messages.
    connection: WebSocketConnection,
}

impl KeepAliveSender {
    /// Sends a keep-alive message over the WebSocket connection.
    ///
    /// This function sends a "KEEPALIVE" message to the specified channel.  Keep-alive messages
    /// are used to maintain the connection and prevent timeouts.
    ///
    /// # Arguments
    ///
    /// * `channel` - The channel ID to send the keep-alive message to.
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the message was sent successfully.
    /// * `Err(DXLinkError)` if there was an error sending the message.  This can occur if
    ///   there is a problem with the WebSocket connection or serializing the message.
    ///
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
mod redaction_tests {
    use super::*;
    use crate::messages::{AuthMessage, KeepaliveMessage};
    use futures_util::StreamExt;
    use std::sync::Arc;
    use tracing_subscriber::fmt::MakeWriter;
    use warp::Filter;

    #[test]
    fn test_redact_sensitive_masks_auth_token() {
        let auth_msg = AuthMessage {
            channel: 0,
            message_type: "AUTH".to_string(),
            token: "super-secret-token".to_string(),
        };
        let json = serde_json::to_string(&auth_msg).unwrap();

        let redacted = redact_sensitive(&json);

        assert!(!redacted.contains("super-secret-token"));
        let value: serde_json::Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(value["token"], REDACTED_PLACEHOLDER);
        assert_eq!(value["type"], "AUTH");
        assert_eq!(value["channel"], 0);
    }

    #[test]
    fn test_redact_sensitive_masks_nested_tokens() {
        let json =
            r#"{"outer":{"token":"secret-a"},"list":[{"Token":"secret-b"}],"safe":"visible"}"#;

        let redacted = redact_sensitive(json);

        assert!(!redacted.contains("secret-a"));
        assert!(!redacted.contains("secret-b"));
        assert!(redacted.contains("visible"));
    }

    #[test]
    fn test_redact_sensitive_leaves_other_messages_untouched() {
        let keepalive = KeepaliveMessage {
            channel: 3,
            message_type: "KEEPALIVE".to_string(),
        };
        let json = serde_json::to_string(&keepalive).unwrap();

        let redacted = redact_sensitive(&json);

        let value: serde_json::Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(value["channel"], 3);
        assert_eq!(value["type"], "KEEPALIVE");
        assert!(!redacted.contains(REDACTED_PLACEHOLDER));
    }

    #[test]
    fn test_redact_sensitive_withholds_unparseable_payloads() {
        assert_eq!(redact_sensitive("not json"), REDACTED_PLACEHOLDER);
    }

    /// A `MakeWriter` that captures log output into a shared buffer so tests
    /// can assert on what was actually logged.
    #[derive(Clone, Default)]
    struct SharedLogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

    impl SharedLogBuffer {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl std::io::Write for SharedLogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedLogBuffer {
        type Writer = SharedLogBuffer;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test]
    async fn test_auth_token_never_appears_in_debug_logs() {
        let buffer = SharedLogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(buffer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        // Minimal WebSocket server on an ephemeral port that swallows messages.
        let websocket = warp::path("websocket")
            .and(warp::ws())
            .map(|ws: warp::ws::Ws| {
                ws.on_upgrade(|mut socket| async move { while socket.next().await.is_some() {} })
            });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("Failed to bind test server");
        let addr = listener.local_addr().expect("Failed to get local addr");
        tokio::spawn(warp::serve(websocket).incoming(listener).run());

        let connection = WebSocketConnection::connect(&format!("ws://{}/websocket", addr))
            .await
            .expect("Failed to connect");

        let token = "tastytrade-live-bearer-token";
        let auth_msg = AuthMessage {
            channel: 0,
            message_type: "AUTH".to_string(),
            token: token.to_string(),
        };
        connection
            .send(&auth_msg)
            .await
            .expect("Failed to send auth message");

        let logs = buffer.contents();
        assert!(logs.contains("Sending message"));
        assert!(!logs.contains(token), "auth token leaked into logs: {logs}");
        assert!(logs.contains(REDACTED_PLACEHOLDER));
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

        // Use a fixed port for testing
        let addr = ([127, 0, 0, 1], 3030).into();
        let server = warp::serve(websocket).run(addr);

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
    #[ignore] // Temporarily disabled due to port conflicts
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

        // Start server on fixed port for testing
        let addr = ([127, 0, 0, 1], 3031).into();
        let server = warp::serve(websocket).run(addr);

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
            while rx.recv().await.is_some() {
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
    #[ignore] // Temporarily disabled due to port conflicts
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
    #[ignore] // Temporarily disabled due to port conflicts
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
    #[ignore] // Temporarily disabled due to port conflicts
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
    #[ignore] // Temporarily disabled due to port conflicts
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
    #[ignore] // Temporarily disabled due to port conflicts
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
