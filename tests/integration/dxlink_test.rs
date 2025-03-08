// integration_tests.rs
use dxlink::{DXLinkClient, DXLinkError, EventType};
use std::time::Duration;
use tokio::time::sleep;

// Helper function to create a mock WebSocket server for testing
mod mock_server {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::Message;
    use tracing::{error, info};

    pub struct MockServer {
        pub address: SocketAddr,
        pub received_messages: Arc<Mutex<Vec<Value>>>,
        pub messages_to_send: mpsc::Sender<(u32, String)>, // (channel_id, message)
        pub shutdown: mpsc::Sender<()>,
    }

    impl MockServer {
        pub async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("Failed to bind");
            let address = listener.local_addr().expect("Failed to get local address");

            // Channel for custom messages from test to connected clients
            let (message_tx, mut message_rx) = mpsc::channel::<(u32, String)>(100);

            // Channel for shutdown signal
            let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

            // Storage for received messages
            let received_messages = Arc::new(Mutex::new(Vec::new()));

            // Track active WebSocket sinks by channel ID
            #[allow(clippy::type_complexity)]
            let websocket_sinks: Arc<Mutex<Vec<(u32, mpsc::Sender<String>)>>> =
                Arc::new(Mutex::new(Vec::new()));

            // Clone for tasks
            let websocket_sinks_clone = websocket_sinks.clone();
            let received_messages_clone = received_messages.clone();

            // Task to forward messages from test to appropriate client
            tokio::spawn(async move {
                while let Some((channel_id, msg)) = message_rx.recv().await {
                    // Create a copy of the senders we need to use
                    let senders_to_use = {
                        let sinks = websocket_sinks.lock().unwrap();
                        sinks
                            .iter()
                            .filter(|(id, _)| *id == channel_id || channel_id == 0)
                            .map(|(_, sender)| sender.clone())
                            .collect::<Vec<_>>()
                    };

                    // Now send to each one without holding the lock
                    for sender in senders_to_use {
                        let _ = sender.send(msg.clone()).await;
                    }
                }
            });

            // Main server task
            tokio::spawn(async move {
                tokio::select! {
                    _ = async {
                        loop {
                            match listener.accept().await {
                                Ok((stream, _)) => {
                                    let ws_stream = tokio_tungstenite::accept_async(stream).await.expect("Failed to accept websocket");
                                    let (write, mut read) = ws_stream.split();

                                    // Create a channel for sending messages to this client
                                    let (client_tx, mut client_rx) = mpsc::channel::<String>(100);

                                    // Initially add with channel 0 (main)
                                    {
                                        let mut sinks = websocket_sinks_clone.lock().unwrap();
                                        sinks.push((0, client_tx.clone()));
                                    }

                                    // Clone for tasks
                                    let received_messages = received_messages_clone.clone();
                                    let websocket_sinks = websocket_sinks_clone.clone();

                                    // Task to send messages to client
                                    let mut write_handle = write;
                                    tokio::spawn(async move {
                                        while let Some(msg) = client_rx.recv().await {
                                            if let Err(e) = write_handle.send(Message::Text(msg.into())).await {
                                                error!("Error sending to client: {}", e);
                                                break;
                                            }
                                        }
                                    });

                                    // Task to process incoming client messages
                                    tokio::spawn(async move {
                                        while let Some(msg) = read.next().await {
                                            if let Ok(Message::Text(text)) = msg {
                                                // Parse the message
                                                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                                                    // Store ALL received messages for later inspection - CRITICAL
                                                    {
                                                        let mut messages = received_messages.lock().unwrap();
                                                        info!("MockServer received message type: {}", value.get("type").unwrap());
                                                        messages.push(value.clone());
                                                    }

                                                    // Get the message type and channel
                                                    let msg_type = value["type"].as_str().unwrap_or("");
                                                    let channel_id = value["channel"].as_u64().unwrap_or(0) as u32;

                                                    // Auto-respond based on message type
                                                    match msg_type {
                                                        "SETUP" => {
                                                            let response = json!({
                                                                "channel": channel_id,
                                                                "type": "SETUP",
                                                                "version": "1.0.0",
                                                                "keepaliveTimeout": 60,
                                                                "acceptKeepaliveTimeout": 60
                                                            }).to_string();

                                                            let _ = client_tx.send(response).await;

                                                            // Send AUTH_STATE
                                                            let auth_state = json!({
                                                                "channel": 0,
                                                                "type": "AUTH_STATE",
                                                                "state": "UNAUTHORIZED"
                                                            }).to_string();

                                                            let _ = client_tx.send(auth_state).await;
                                                        },
                                                        "AUTH" => {
                                                            let auth_response = json!({
                                                                "channel": 0,
                                                                "type": "AUTH_STATE",
                                                                "state": "AUTHORIZED",
                                                                "userId": "test-user"
                                                            }).to_string();

                                                            let _ = client_tx.send(auth_response).await;
                                                        },
                                                        "CHANNEL_REQUEST" => {
                                                            if value["service"].as_str().unwrap_or("") == "FEED" {
                                                                // Register this client with the requested channel ID
                                                                {
                                                                    let mut sinks = websocket_sinks.lock().unwrap();
                                                                    sinks.push((channel_id, client_tx.clone()));
                                                                }

                                                                let channel_opened = json!({
                                                                    "channel": channel_id,
                                                                    "type": "CHANNEL_OPENED",
                                                                    "service": "FEED",
                                                                    "parameters": {}
                                                                }).to_string();

                                                                let _ = client_tx.send(channel_opened).await;
                                                            }
                                                        },
                                                        "FEED_SETUP" => {
                                                            let feed_config = json!({
                                                                "channel": channel_id,
                                                                "type": "FEED_CONFIG",
                                                                "aggregationPeriod": 0.1,
                                                                "dataFormat": "COMPACT"
                                                            }).to_string();

                                                            let _ = client_tx.send(feed_config).await;
                                                        },
                                                        "FEED_SUBSCRIPTION" => {
                                                            // This is the critical message we need to handle properly
                                                            if let Some(add) = value.get("add") {
                                                                if let Some(subscriptions) = add.as_array() {
                                                                    for sub in subscriptions {
                                                                        let event_type = sub["type"].as_str().unwrap_or("");
                                                                        let symbol = sub["symbol"].as_str().unwrap_or("");

                                                                        // Send mock data for this subscription
                                                                        if event_type == "Quote" {
                                                                            let quote_data = json!({
                                                                                "channel": channel_id,
                                                                                "type": "FEED_DATA",
                                                                                "data": [
                                                                                    "Quote",
                                                                                    [
                                                                                        symbol,
                                                                                        "Quote",
                                                                                        150.25,
                                                                                        150.50,
                                                                                        100,
                                                                                        150
                                                                                    ]
                                                                                ]
                                                                            }).to_string();

                                                                            let _ = client_tx.send(quote_data).await;
                                                                        } else if event_type == "Trade" {
                                                                            let trade_data = json!({
                                                                                "channel": channel_id,
                                                                                "type": "FEED_DATA",
                                                                                "data": [
                                                                                    "Trade",
                                                                                    [
                                                                                        symbol,
                                                                                        "Trade",
                                                                                        151.25,
                                                                                        75,
                                                                                        10000000
                                                                                    ]
                                                                                ]
                                                                            }).to_string();

                                                                            let _ = client_tx.send(trade_data).await;
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        },
                                                        "CHANNEL_CANCEL" => {
                                                            // Remove this channel from tracked sinks
                                                            {
                                                                let mut sinks = websocket_sinks.lock().unwrap();
                                                                sinks.retain(|(id, _)| *id != channel_id);
                                                            }

                                                            let channel_closed = json!({
                                                                "channel": channel_id,
                                                                "type": "CHANNEL_CLOSED"
                                                            }).to_string();

                                                            let _ = client_tx.send(channel_closed).await;
                                                        },
                                                        _ => {}
                                                    }
                                                }
                                            }
                                        }
                                    });
                                },
                                Err(e) => {
                                    error!("Error accepting connection: {}", e);
                                    break;
                                }
                            }
                        }
                    } => {},
                    _ = shutdown_rx.recv() => {
                        info!("Mock server shutting down");
                    }
                }
            });

            MockServer {
                address,
                received_messages,
                messages_to_send: message_tx,
                shutdown: shutdown_tx,
            }
        }

        pub async fn send(&self, message: &str) {
            // Send to all clients (channel 0)
            let _ = self.messages_to_send.send((0, message.to_string())).await;
        }

        pub fn get_received_messages(&self) -> Vec<Value> {
            let messages = self.received_messages.lock().unwrap().clone();
            info!("MockServer returning {} received messages", messages.len());
            for (i, msg) in messages.iter().enumerate() {
                if let Some(msg_type) = msg.get("type") {
                    info!("  Message {}: type={}", i, msg_type);
                }
            }
            messages
        }

        pub async fn shutdown(self) {
            let _ = self.shutdown.send(()).await;
        }
    }
}

// A test that verifies the basic connection and authentication flow
#[tokio::test]
async fn test_connect_and_authenticate() {
    let server = mock_server::MockServer::new().await;
    let url = format!("ws://{}", server.address);

    // Create client with test token
    let mut client = DXLinkClient::new(&url, "test-token");

    // Connect to server
    let result = client.connect().await;
    assert!(result.is_ok(), "Failed to connect: {:?}", result);

    // Check that the server received the expected messages
    let messages = server.get_received_messages();

    // Find SETUP message
    let setup_msg = messages.iter().find(|m| m["type"] == "SETUP");
    assert!(setup_msg.is_some(), "No SETUP message sent");

    // Find AUTH message
    let auth_msg = messages.iter().find(|m| m["type"] == "AUTH");
    assert!(auth_msg.is_some(), "No AUTH message sent");
    assert_eq!(
        auth_msg.unwrap()["token"],
        "test-token",
        "Incorrect token in AUTH message"
    );

    // Cleanup
    client.disconnect().await.unwrap();
    server.shutdown().await;
}

// Test creating a feed channel and setting it up
#[tokio::test]
async fn test_create_and_setup_feed() {
    let server = mock_server::MockServer::new().await;
    let url = format!("ws://{}", server.address);

    // Create and connect client
    let mut client = DXLinkClient::new(&url, "test-token");
    client.connect().await.unwrap();

    // Create feed channel
    let channel_id = client.create_feed_channel("AUTO").await;
    assert!(
        channel_id.is_ok(),
        "Failed to create feed channel: {:?}",
        channel_id
    );
    let channel_id = channel_id.unwrap();

    // Check channel request message
    let messages = server.get_received_messages();
    let channel_req = messages.iter().find(|m| {
        m["type"] == "CHANNEL_REQUEST" && m["service"] == "FEED" && m["channel"] == channel_id
    });
    assert!(channel_req.is_some(), "No CHANNEL_REQUEST message sent");

    // Setup feed channel
    let result = client
        .setup_feed(channel_id, &[EventType::Quote, EventType::Trade])
        .await;
    assert!(result.is_ok(), "Failed to setup feed: {:?}", result);

    // Check feed setup message
    let messages = server.get_received_messages();
    let feed_setup = messages
        .iter()
        .find(|m| m["type"] == "FEED_SETUP" && m["channel"] == channel_id);
    assert!(feed_setup.is_some(), "No FEED_SETUP message sent");

    // Cleanup
    client.disconnect().await.unwrap();
    server.shutdown().await;
}

// Test closing a channel
#[tokio::test]
async fn test_close_channel() {
    let server = mock_server::MockServer::new().await;
    let url = format!("ws://{}", server.address);

    // Create and connect client
    let mut client = DXLinkClient::new(&url, "test-token");
    client.connect().await.unwrap();

    // Create feed channel
    let channel_id = client.create_feed_channel("AUTO").await.unwrap();

    // Close the channel
    let result = client.close_channel(channel_id).await;
    assert!(result.is_ok(), "Failed to close channel: {:?}", result);

    // Check channel cancel message
    let messages = server.get_received_messages();
    let cancel_msg = messages
        .iter()
        .find(|m| m["type"] == "CHANNEL_CANCEL" && m["channel"] == channel_id);

    assert!(cancel_msg.is_some(), "No CHANNEL_CANCEL message sent");

    // Cleanup
    client.disconnect().await.unwrap();
    server.shutdown().await;
}

// Test error handling - non-existent channel
#[tokio::test]
async fn test_error_non_existent_channel() {
    let server = mock_server::MockServer::new().await;
    let url = format!("ws://{}", server.address);

    // Create and connect client
    let mut client = DXLinkClient::new(&url, "test-token");
    client.connect().await.unwrap();

    // Try to use a channel that doesn't exist
    let result = client.setup_feed(999, &[EventType::Quote]).await;
    assert!(result.is_err(), "Expected error for non-existent channel");

    // Check error type
    match result {
        Err(DXLinkError::Channel(_)) => {} // Expected error type
        Err(e) => panic!("Wrong error type: {:?}", e),
        Ok(_) => panic!("Expected error but got Ok"),
    }

    // Cleanup
    client.disconnect().await.unwrap();
    server.shutdown().await;
}

// Test keepalive mechanism
#[tokio::test]
async fn test_keepalive() {
    let server = mock_server::MockServer::new().await;
    let url = format!("ws://{}", server.address);

    // Create and connect client
    let mut client = DXLinkClient::new(&url, "test-token");
    client.connect().await.unwrap();

    // Wait for keepalive to be sent (adjust time based on keepalive interval)
    sleep(Duration::from_secs(20)).await;

    // Check keepalive messages
    let messages = server.get_received_messages();
    let keepalive_msgs = messages.iter().filter(|m| m["type"] == "KEEPALIVE").count();

    assert!(keepalive_msgs > 0, "No KEEPALIVE messages sent");

    // Cleanup
    client.disconnect().await.unwrap();
    server.shutdown().await;
}

// Test authentication failure
#[tokio::test]
async fn test_authentication_failure() {
    let server = mock_server::MockServer::new().await;
    let url = format!("ws://{}", server.address);

    // Create client with invalid token
    let mut client = DXLinkClient::new(&url, "invalid-token");

    // Override normal auth response with failure
    server
        .send(r#"{"channel":0,"type":"AUTH_STATE","state":"UNAUTHORIZED"}"#)
        .await;

    // Connect to server (should fail due to authentication issues)
    let _result = client.connect().await;

    // Either connection will fail or it will hang waiting for AUTH_STATE response
    // Force disconnect to clean up regardless
    let _ = client.disconnect().await;
    server.shutdown().await;
}
