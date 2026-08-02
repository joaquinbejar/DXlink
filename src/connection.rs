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

/// How long establishing the WebSocket may take before it is abandoned.
///
/// Without a bound, a server that accepts the TCP connection and then never
/// completes the upgrade holds `connect()` open indefinitely.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Placeholder written in place of credential values when logging outbound messages.
const REDACTED_PLACEHOLDER: &str = "<redacted>";

/// Maximum number of characters kept from a server-supplied close reason.
const MAX_CLOSE_REASON_LEN: usize = 120;

/// Longest unbroken run kept verbatim in a close reason. Real reasons are made
/// of short words; anything longer has the shape of a credential.
const MAX_CLOSE_REASON_WORD_LEN: usize = 32;

/// Returns server-supplied text that is safe to log and to embed in an error.
///
/// The reason is free text chosen by the server, and it reaches both the logs
/// and [`DXLinkError::Connection`]. The close path is also the authentication
/// failure path, so a server that echoed the auth token back would turn our own
/// error reporting into a credential leak. Control characters are dropped, runs
/// too long to be a word are masked with [`REDACTED_PLACEHOLDER`], and the
/// result is truncated. The close code is a protocol enum and is always kept.
pub(crate) fn sanitize_server_text(reason: &str) -> String {
    let printable: String = reason.chars().filter(|c| !c.is_control()).collect();

    let masked = printable
        .split_whitespace()
        .map(|word| {
            if word.chars().count() > MAX_CLOSE_REASON_WORD_LEN {
                REDACTED_PLACEHOLDER
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    masked.chars().take(MAX_CLOSE_REASON_LEN).collect()
}

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

        let (ws_stream, _) = match timeout(DEFAULT_CONNECT_TIMEOUT, connect_async(url)).await {
            Ok(result) => {
                result.map_err(|e| DXLinkError::Connection(format!("Failed to connect: {}", e)))?
            }
            Err(_) => {
                return Err(DXLinkError::Timeout(format!(
                    "timed out after {}s establishing the WebSocket connection to {}",
                    DEFAULT_CONNECT_TIMEOUT.as_secs(),
                    url
                )));
            }
        };

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
                    // The code is a protocol enum and is trusted; the reason is
                    // free text from the server and is not.
                    let detail = match frame {
                        Some(frame) => format!(
                            "code {}, reason: {}",
                            frame.code,
                            sanitize_server_text(&frame.reason)
                        ),
                        None => "no close frame".to_string(),
                    };
                    error!("Server closed the connection: {}", detail);

                    // Closing is a handshake. tungstenite queues the reply when
                    // this frame is yielded, but on a split stream it only
                    // reaches the peer once the write half is driven, so do that
                    // before giving up on the socket. Failing here changes
                    // nothing for the caller: the connection is gone either way.
                    if let Err(e) = self.write.lock().await.close().await {
                        debug!("Could not complete the close handshake: {}", e);
                    }

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
mod frame_tests {
    //! Frame-level tests for [`WebSocketConnection::receive`].
    //!
    //! These drive the server side with `tokio_tungstenite::accept_async`, which
    //! is the only way to emit a specific frame kind on demand — a Ping, or a
    //! Close with a chosen code. Every server binds an ephemeral port so the
    //! suite stays parallel-safe; the fixed ports these replaced are why six of
    //! these tests used to be `#[ignore]`d.

    use super::*;
    use futures_util::SinkExt;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    /// Starts a server that sends `frames` to the first client that connects and
    /// then keeps the socket open. Returns the URL to connect to.
    async fn serve_frames(frames: Vec<Message>) -> String {
        serve_frames_after(frames, Duration::ZERO).await
    }

    /// Starts a server that reports every text message it receives and forwards
    /// anything pushed into the returned sender back to the client.
    ///
    /// Returns `(url, received, to_client)`.
    #[allow(clippy::type_complexity)]
    async fn serve_recording() -> (
        String,
        tokio::sync::mpsc::Receiver<String>,
        tokio::sync::mpsc::Sender<String>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("failed to bind test server");
        let addr = listener.local_addr().expect("failed to read local addr");

        let (received_tx, received_rx) = tokio::sync::mpsc::channel::<String>(16);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<String>(16);

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("failed to accept");
            let ws = accept_async(stream).await.expect("failed to handshake");
            let (mut write, mut read) = ws.split();

            tokio::spawn(async move {
                while let Some(text) = outbound_rx.recv().await {
                    if write.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
            });

            while let Some(Ok(message)) = read.next().await {
                if let Message::Text(text) = message
                    && received_tx.send(text.to_string()).await.is_err()
                {
                    return;
                }
            }
        });

        (format!("ws://{}", addr), received_rx, outbound_tx)
    }

    /// Waits for the next message the server recorded, or fails the test.
    async fn next_received(rx: &mut tokio::sync::mpsc::Receiver<String>) -> String {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("server never received a message")
            .expect("server channel closed")
    }

    #[tokio::test]
    async fn test_send_and_receive_round_trip() {
        let (url, mut received, to_client) = serve_recording().await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        #[derive(Serialize)]
        struct TestMessage {
            channel: u32,
            #[serde(rename = "type")]
            message_type: String,
            data: String,
        }

        connection
            .send(&TestMessage {
                channel: 1,
                message_type: "TEST".to_string(),
                data: "Hello, World!".to_string(),
            })
            .await
            .expect("failed to send");

        let raw = next_received(&mut received).await;
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).expect("server got invalid JSON");
        assert_eq!(parsed["channel"], 1);
        assert_eq!(parsed["type"], "TEST");
        assert_eq!(parsed["data"], "Hello, World!");

        to_client
            .send("test_response".to_string())
            .await
            .expect("failed to push a response");
        assert_eq!(
            connection.receive().await.expect("failed to receive"),
            "test_response"
        );
    }

    #[tokio::test]
    async fn test_clone_shares_the_underlying_stream() {
        let (url, _received, to_client) = serve_recording().await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");
        let clone = connection.clone();

        to_client
            .send("first".to_string())
            .await
            .expect("failed to push");
        assert_eq!(
            connection.receive().await.expect("failed to receive"),
            "first"
        );

        // The clone reads from the same stream, so it picks up the next message
        // rather than repeating the first.
        to_client
            .send("second".to_string())
            .await
            .expect("failed to push");
        assert_eq!(clone.receive().await.expect("failed to receive"), "second");
    }

    #[tokio::test]
    async fn test_keepalive_sender_targets_the_requested_channel() {
        let (url, mut received, _to_client) = serve_recording().await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        connection
            .create_keepalive_sender()
            .send_keepalive(5)
            .await
            .expect("failed to send keepalive");

        let parsed: serde_json::Value = serde_json::from_str(&next_received(&mut received).await)
            .expect("server got invalid JSON");
        assert_eq!(parsed["channel"], 5);
        assert_eq!(parsed["type"], "KEEPALIVE");

        // A sender built from a clone shares the same socket.
        connection
            .clone()
            .create_keepalive_sender()
            .send_keepalive(10)
            .await
            .expect("failed to send keepalive from a clone");

        let parsed: serde_json::Value = serde_json::from_str(&next_received(&mut received).await)
            .expect("server got invalid JSON");
        assert_eq!(parsed["channel"], 10);
    }

    /// As [`serve_frames`], but waits `delay` before sending anything.
    async fn serve_frames_after(frames: Vec<Message>, delay: Duration) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("failed to bind test server");
        let addr = listener.local_addr().expect("failed to read local addr");

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("failed to accept");
            let mut ws = accept_async(stream).await.expect("failed to handshake");

            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }

            for frame in frames {
                let is_close = matches!(frame, Message::Close(_));
                ws.send(frame).await.expect("failed to send frame");
                if is_close {
                    return;
                }
            }

            // Hold the connection open so a timeout test observes silence rather
            // than a close.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        format!("ws://{}", addr)
    }

    #[tokio::test]
    async fn test_receive_skips_control_frames_and_returns_following_text() {
        let url = serve_frames(vec![
            Message::Ping(vec![1, 2, 3].into()),
            Message::Pong(Vec::new().into()),
            Message::Text("{\"type\":\"SETUP\"}".into()),
        ])
        .await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        let received = connection
            .receive()
            .await
            .expect("control frames should be skipped, not fatal");
        assert_eq!(received, "{\"type\":\"SETUP\"}");
    }

    #[tokio::test]
    async fn test_receive_reports_close_as_connection_error() {
        let url = serve_frames(vec![Message::Close(Some(CloseFrame {
            code: CloseCode::Policy,
            reason: "invalid token".into(),
        }))])
        .await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        match connection.receive().await {
            Err(DXLinkError::Connection(msg)) => {
                // The reason is what tells an operator why the server hung up,
                // so it has to survive into the error text.
                assert!(msg.contains("invalid token"), "close reason lost: {msg}");
            }
            other => panic!("expected Connection error, got: {:?}", other),
        }
    }

    /// The close path is the authentication failure path, so a server that
    /// echoes the token back must not get it into our error text or our logs.
    #[tokio::test]
    async fn test_close_reason_cannot_leak_a_credential() {
        let token = "tastytrade-live-bearer-token-that-is-long-enough-to-be-a-secret";
        let url = serve_frames(vec![Message::Close(Some(CloseFrame {
            code: CloseCode::Policy,
            reason: format!("rejected token {token}").into(),
        }))])
        .await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        let err = connection
            .receive()
            .await
            .expect_err("a close frame must not be reported as success");

        let text = err.to_string();
        assert!(!text.contains(token), "token leaked into the error: {text}");
        assert!(text.contains(REDACTED_PLACEHOLDER));
        // The close code still has to survive, it is the actionable part.
        assert!(text.contains("code"), "close code lost: {text}");
    }

    #[tokio::test]
    async fn test_close_error_is_terminal() {
        let url = serve_frames(vec![Message::Close(None)]).await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        let err = connection
            .receive()
            .await
            .expect_err("a close frame must not be reported as success");
        // This is what stops the message task instead of letting it spin.
        assert!(err.is_terminal(), "close should be terminal, got: {err:?}");
    }

    /// Closing is a handshake: when the peer sends Close we must send one back
    /// before giving up on the socket, or the connection is left half closed.
    #[tokio::test]
    async fn test_receive_completes_the_close_handshake() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("failed to bind test server");
        let addr = listener.local_addr().expect("failed to read local addr");

        let echoed = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("failed to accept");
            let mut ws = accept_async(stream).await.expect("failed to handshake");

            ws.send(Message::Close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "bye".into(),
            })))
            .await
            .expect("failed to send close");

            // Wait for the client's half of the close handshake.
            loop {
                match ws.next().await {
                    Some(Ok(Message::Close(_))) | None => return true,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => return false,
                }
            }
        });

        let connection = WebSocketConnection::connect(&format!("ws://{}", addr))
            .await
            .expect("failed to connect");

        let _ = connection.receive().await;

        let replied = tokio::time::timeout(Duration::from_secs(3), echoed)
            .await
            .expect("server never saw the client's close reply")
            .expect("server task panicked");
        assert!(replied, "client did not complete the close handshake");
    }

    #[tokio::test]
    async fn test_receive_rejects_binary_frames() {
        let url = serve_frames(vec![Message::Binary(vec![1, 2, 3].into())]).await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        match connection.receive().await {
            Err(DXLinkError::UnexpectedMessage(msg)) => {
                assert!(msg.contains("binary"), "unhelpful error text: {msg}");
            }
            other => panic!("expected UnexpectedMessage error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_binary_error_is_not_terminal() {
        let url = serve_frames(vec![Message::Binary(vec![0xff].into())]).await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        let err = connection
            .receive()
            .await
            .expect_err("a binary frame must be rejected");
        // One bad frame does not mean the connection is gone.
        assert!(
            !err.is_terminal(),
            "binary frame should not kill the connection: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_receive_with_timeout_is_not_woken_by_control_frames() {
        // Ping first, payload only after a delay: a control frame must not make
        // the call return early with nothing useful.
        let url = serve_frames_after(
            vec![
                Message::Ping(Vec::new().into()),
                Message::Text("payload".into()),
            ],
            Duration::from_millis(50),
        )
        .await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        let received = connection
            .receive_with_timeout(Duration::from_secs(5))
            .await
            .expect("receive_with_timeout failed");
        assert_eq!(received, Some("payload".to_string()));
    }

    #[tokio::test]
    async fn test_receive_with_timeout_returns_none_on_silence() {
        let url = serve_frames(Vec::new()).await;

        let connection = WebSocketConnection::connect(&url)
            .await
            .expect("failed to connect");

        let received = connection
            .receive_with_timeout(Duration::from_millis(100))
            .await
            .expect("receive_with_timeout failed");
        assert_eq!(received, None);
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::*;
    use crate::messages::{AuthMessage, KeepaliveMessage};
    use std::sync::Arc;
    use tracing_subscriber::fmt::MakeWriter;

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

    #[test]
    fn test_sanitize_server_text_keeps_ordinary_text() {
        // The diagnostic value of a close reason is the whole point of keeping
        // it, so normal wording must survive untouched.
        assert_eq!(
            sanitize_server_text("invalid token, please re-authenticate"),
            "invalid token, please re-authenticate"
        );
    }

    #[test]
    fn test_sanitize_server_text_masks_credential_shaped_runs() {
        let token = "a".repeat(64);
        let sanitized = sanitize_server_text(&format!("rejected token {token} for user bob"));

        assert!(!sanitized.contains(&token), "token survived: {sanitized}");
        assert!(sanitized.contains(REDACTED_PLACEHOLDER));
        // Everything that is not credential-shaped is still readable.
        assert!(sanitized.contains("rejected token"));
        assert!(sanitized.contains("for user bob"));
    }

    #[test]
    fn test_sanitize_server_text_drops_control_characters() {
        let sanitized = sanitize_server_text("bad\u{0}token\nsecond line\u{7}");

        assert!(!sanitized.contains('\u{0}'));
        assert!(!sanitized.contains('\u{7}'));
        assert!(!sanitized.contains('\n'));
    }

    #[test]
    fn test_sanitize_server_text_is_bounded() {
        // Many short words: nothing is credential-shaped, so only the overall
        // length limit applies.
        let long = "word ".repeat(200);
        let sanitized = sanitize_server_text(&long);

        assert_eq!(sanitized.chars().count(), MAX_CLOSE_REASON_LEN);
    }

    #[test]
    fn test_sanitize_server_text_handles_empty_input() {
        assert_eq!(sanitize_server_text(""), "");
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

    /// Captures `tracing` output for the whole test binary.
    ///
    /// A per-test `set_default` subscriber is thread-local, so once the runtime
    /// resumed a test future on another worker the capture stopped and the
    /// assertions ran against a buffer missing the very line they check. One
    /// global subscriber, installed once, has no such race. Tests scope
    /// themselves by searching for their own unique marker.
    fn captured_logs() -> &'static SharedLogBuffer {
        static BUFFER: std::sync::OnceLock<SharedLogBuffer> = std::sync::OnceLock::new();
        BUFFER.get_or_init(|| {
            let buffer = SharedLogBuffer::default();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(buffer.clone())
                .finish();
            // If some other code already installed a global subscriber this call
            // fails and the buffer stays empty. That is why the test asserts it
            // captured something before asserting on the contents: it fails
            // loudly rather than passing vacuously.
            let _ = tracing::subscriber::set_global_default(subscriber);
            buffer
        })
    }

    #[tokio::test]
    async fn test_auth_token_never_appears_in_debug_logs() {
        let logs = captured_logs();
        let token = "tastytrade-live-bearer-token-unique-to-this-test";

        // Minimal WebSocket server on an ephemeral port that swallows messages.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("Failed to bind test server");
        let addr = listener.local_addr().expect("Failed to get local addr");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("failed to accept");
            let mut ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("failed to handshake");
            while ws.next().await.is_some() {}
        });

        let connection = WebSocketConnection::connect(&format!("ws://{}", addr))
            .await
            .expect("Failed to connect");

        connection
            .send(&AuthMessage {
                channel: 0,
                message_type: "AUTH".to_string(),
                token: token.to_string(),
            })
            .await
            .expect("Failed to send auth message");

        let contents = logs.contents();
        let auth_lines: Vec<&str> = contents
            .lines()
            .filter(|line| line.contains("Sending message") && line.contains("\"type\":\"AUTH\""))
            .collect();

        assert!(
            !auth_lines.is_empty(),
            "no outbound AUTH was logged, the assertions below would be vacuous"
        );
        assert!(
            !contents.contains(token),
            "auth token leaked into logs: {contents}"
        );
        for line in auth_lines {
            assert!(
                line.contains(REDACTED_PLACEHOLDER),
                "an AUTH payload was logged without redaction: {line}"
            );
        }
    }
}
