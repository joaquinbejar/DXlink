//! Protocol-level integration tests: what the client puts on the wire, and how
//! it reacts to what comes back.
//!
//! Event delivery is covered in `dxlink_flow.rs`.

use crate::fixture::{Behaviour, MockServer, is_type, is_type_on_channel};
use dxlink::{DXLinkClient, DXLinkError, EventType};
use std::time::Duration;

#[tokio::test]
async fn test_connect_and_authenticate() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    let setup = server.wait_for("SETUP", is_type("SETUP")).await;
    assert_eq!(setup["channel"], 0, "SETUP must go out on the main channel");
    assert!(
        setup["version"].as_str().is_some_and(|v| !v.is_empty()),
        "SETUP must carry a client version"
    );

    let auth = server.wait_for("AUTH", is_type("AUTH")).await;
    assert_eq!(auth["token"], "test-token");
    assert_eq!(auth["channel"], 0);

    client.disconnect().await.expect("failed to disconnect");
}

/// A rejected token must surface as an authentication error, not as a hang and
/// not as a success.
#[tokio::test]
async fn test_authentication_failure_is_reported() {
    let server = MockServer::start(Behaviour::RejectAuth).await;

    let mut client = DXLinkClient::new(&server.url(), "invalid-token");

    let result = tokio::time::timeout(Duration::from_secs(5), client.connect())
        .await
        .expect("connect hung on a rejected token");

    match result {
        Err(DXLinkError::Authentication(msg)) => {
            assert!(
                msg.contains("UNAUTHORIZED"),
                "error should name the state the server reported: {msg}"
            );
        }
        Err(other) => panic!("expected an Authentication error, got: {other:?}"),
        Ok(_) => panic!("connect succeeded against a server that rejected the token"),
    }
}

#[tokio::test]
async fn test_create_and_setup_feed() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    let request = server
        .wait_for(
            "CHANNEL_REQUEST",
            is_type_on_channel("CHANNEL_REQUEST", channel_id),
        )
        .await;
    assert_eq!(request["service"], "FEED");
    assert_eq!(request["parameters"]["contract"], "AUTO");

    client
        .setup_feed(channel_id, &[EventType::Quote, EventType::Trade])
        .await
        .expect("failed to set up feed");

    let setup = server
        .wait_for("FEED_SETUP", is_type_on_channel("FEED_SETUP", channel_id))
        .await;
    assert_eq!(setup["acceptDataFormat"], "COMPACT");

    // The requested field list is one half of the COMPACT contract; the decoder
    // in utils.rs is the other. Pin it here so a change to either is visible.
    let quote_fields: Vec<&str> = setup["acceptEventFields"]["Quote"]
        .as_array()
        .expect("no Quote field list")
        .iter()
        .filter_map(|f| f.as_str())
        .collect();
    assert_eq!(
        quote_fields,
        vec![
            "eventType",
            "eventSymbol",
            "bidPrice",
            "askPrice",
            "bidSize",
            "askSize"
        ]
    );

    client.disconnect().await.expect("failed to disconnect");
}

#[tokio::test]
async fn test_close_channel() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    client
        .close_channel(channel_id)
        .await
        .expect("failed to close channel");

    server
        .wait_for(
            "CHANNEL_CANCEL",
            is_type_on_channel("CHANNEL_CANCEL", channel_id),
        )
        .await;

    // A closed channel is no longer usable.
    let result = client.setup_feed(channel_id, &[EventType::Quote]).await;
    assert!(
        matches!(result, Err(DXLinkError::Channel(_))),
        "a closed channel should be rejected, got: {result:?}"
    );

    client.disconnect().await.expect("failed to disconnect");
}

#[tokio::test]
async fn test_error_non_existent_channel() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    let result = client.setup_feed(999, &[EventType::Quote]).await;
    assert!(
        matches!(result, Err(DXLinkError::Channel(_))),
        "expected a Channel error for an unknown channel, got: {result:?}"
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// The client must keep the session alive on its own. Virtual time drives the
/// interval so the suite does not spend 20 real seconds proving it.
#[tokio::test(start_paused = true)]
async fn test_client_sends_keepalives() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    // Past one keepalive interval; with time paused this costs nothing.
    tokio::time::advance(Duration::from_secs(20)).await;

    // Back to real time before waiting: delivering the keepalive is real socket
    // I/O, and a virtual-time timeout would expire before it lands.
    tokio::time::resume();

    server.wait_for("KEEPALIVE", is_type("KEEPALIVE")).await;

    client.disconnect().await.expect("failed to disconnect");
}
