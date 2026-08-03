//! Protocol-level integration tests: what the client puts on the wire, and how
//! it reacts to what comes back.
//!
//! Event delivery is covered in `dxlink_flow.rs`.

use crate::fixture::{Behaviour, MockServer, expected, is_type, is_type_on_channel};
use dxlink::{DXLinkClient, DXLinkError, EventType, FeedSubscription, MarketEvent};
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

/// The client must keep the session alive on the deadline the server
/// negotiated, not on a fixed schedule of its own. The fixture asks for 3
/// seconds here, below the 15 the client used to assume: with the old fixed
/// interval the server would have timed the connection out before the first
/// keepalive went out.
#[tokio::test]
async fn test_client_honors_a_short_negotiated_keepalive() {
    let server = MockServer::start(Behaviour::ShortKeepalive).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    // 3s deadline means maintenance every second; well inside wait_for's bound.
    server.wait_for("KEEPALIVE", is_type("KEEPALIVE")).await;

    client.disconnect().await.expect("failed to disconnect");
}

/// The first tick of a tokio interval is immediate, which used to fire a
/// redundant KEEPALIVE the instant the session opened.
#[tokio::test]
async fn test_no_keepalive_is_sent_immediately_after_connecting() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    // The handshake is done; nothing should follow it straight away.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let types: Vec<String> = server
        .received()
        .iter()
        .filter_map(|m| m["type"].as_str().map(String::from))
        .collect();
    assert!(
        !types.contains(&"KEEPALIVE".to_string()),
        "a keepalive went out immediately after connecting: {types:?}"
    );

    client.disconnect().await.expect("failed to disconnect");
}

// --- Handshake validation (issue #11) -------------------------------------
//
// The handshake used to deserialize whatever arrived straight into the type it
// hoped for, so a server ERROR surfaced as `missing field \`state\`` and a reply
// on the wrong channel was accepted silently.

/// A server that accepts the socket and says nothing must not hold the caller
/// open. Virtual time makes the bound observable without waiting for it.
#[tokio::test(start_paused = true)]
async fn test_handshake_times_out_on_a_silent_server() {
    let server = MockServer::start(Behaviour::Silent).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");

    match client.connect().await {
        // Either bound is a correct outcome: what matters is that one of them
        // fired instead of hanging.
        Err(DXLinkError::Timeout(msg)) => {
            assert!(
                msg.contains("timed out"),
                "timeout should say what it waited for: {msg}"
            );
        }
        other => panic!("expected a Timeout against a silent server, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_handshake_rejects_the_wrong_message_type() {
    let server = MockServer::start(Behaviour::WrongTypeOnSetup).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");

    match client.connect().await {
        Err(DXLinkError::Protocol(msg)) => {
            assert!(msg.contains("SETUP"), "expected type missing: {msg}");
            assert!(msg.contains("FEED_CONFIG"), "received type missing: {msg}");
        }
        other => panic!("expected a Protocol error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_handshake_rejects_the_wrong_channel() {
    let server = MockServer::start(Behaviour::WrongChannelOnSetup).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");

    match client.connect().await {
        Err(DXLinkError::Protocol(msg)) => {
            assert!(msg.contains("channel 0"), "expected channel missing: {msg}");
            assert!(msg.contains("channel 7"), "received channel missing: {msg}");
        }
        other => panic!("expected a Protocol error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_handshake_rejects_malformed_json() {
    let server = MockServer::start(Behaviour::MalformedSetup).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");

    match client.connect().await {
        Err(DXLinkError::Protocol(msg)) => {
            assert!(msg.contains("malformed JSON"), "unclear error: {msg}");
        }
        other => panic!("expected a Protocol error, got: {other:?}"),
    }
}

/// The failure the reporter of #4 actually hit next: no token, so the server
/// answers ERROR/UNAUTHORIZED. That used to surface as `missing field \`state\``.
#[tokio::test]
async fn test_handshake_reports_a_server_error_as_authentication() {
    let server = MockServer::start(Behaviour::ErrorOnSetup).await;
    let mut client = DXLinkClient::new(&server.url(), "");

    match client.connect().await {
        Err(DXLinkError::Authentication(msg)) => {
            assert!(msg.contains("UNAUTHORIZED"), "error code missing: {msg}");
            assert!(
                msg.contains("Authentication failed"),
                "server message missing: {msg}"
            );
        }
        other => panic!("expected an Authentication error, got: {other:?}"),
    }
}

/// A failed handshake must leave the client disconnected, not half-established.
#[tokio::test]
async fn test_failed_handshake_leaves_the_client_disconnected() {
    let server = MockServer::start(Behaviour::ErrorOnSetup).await;
    let mut client = DXLinkClient::new(&server.url(), "");

    assert!(client.connect().await.is_err());

    // No channel can be opened, and tearing down is still safe.
    assert!(client.create_feed_channel("AUTO").await.is_err());
    client
        .disconnect()
        .await
        .expect("disconnect after a failed handshake should be safe");
}

/// A server that echoes the credential back in its error message must not get
/// it into an error the caller will log.
#[tokio::test]
async fn test_handshake_error_cannot_leak_an_echoed_token() {
    let token = "tastytrade-live-bearer-token-long-enough-to-be-a-secret";
    let server = MockServer::start(Behaviour::ErrorEchoingToken).await;
    let mut client = DXLinkClient::new(&server.url(), token);

    let err = client
        .connect()
        .await
        .expect_err("the server rejected the handshake");

    let text = err.to_string();
    assert!(!text.contains(token), "token leaked into the error: {text}");
    // The actionable part survives.
    assert!(text.contains("UNAUTHORIZED"), "error code lost: {text}");
}

// --- Response routing (issue #9) -------------------------------------------

/// A channel-scoped ERROR must answer whatever operation is pending on that
/// channel. It used to be logged only, so the caller sat until its timeout and
/// got a misleading Timeout instead of the reason the server gave.
#[tokio::test]
async fn test_channel_error_answers_the_pending_operation() {
    let server = MockServer::start(Behaviour::ErrorOnChannelRequest).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    let started = std::time::Instant::now();
    let result = client.create_feed_channel("AUTO").await;

    match result {
        Err(DXLinkError::Protocol(msg)) => {
            assert!(msg.contains("BAD_ACTION"), "server error code lost: {msg}");
            assert!(
                msg.contains("create_feed_channel"),
                "operation context lost: {msg}"
            );
        }
        other => panic!("expected the server error to be delivered, got: {other:?}"),
    }

    // The point is that it did not wait out the 10 second timeout.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the error was not delivered promptly: {:?}",
        started.elapsed()
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// A request that times out must leave nothing behind that could consume a
/// later response. Two timed-out attempts followed by a working one is the
/// shape that used to break: the stale entry stole the third response.
#[tokio::test]
async fn test_a_timed_out_request_cannot_steal_a_later_response() {
    let server = MockServer::start(Behaviour::IgnoreChannelRequest).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    // Cancel the wait rather than sitting through the full timeout; a dropped
    // future is exactly the path that used to leak a registration.
    for _ in 0..2 {
        let cancelled = tokio::time::timeout(
            Duration::from_millis(200),
            client.create_feed_channel("AUTO"),
        )
        .await;
        assert!(cancelled.is_err(), "the fixture should never answer");
    }

    // Nothing stale may remain to answer for a later request.
    let pending = client.pending_response_count();
    assert_eq!(
        pending, 0,
        "{pending} registrations survived a cancelled wait"
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// The helper's own timeout is a distinct cleanup path from a cancelled caller.
/// This lets it fire, then proves the stale registration cannot answer for the
/// request that follows: the second attempt must get its own channel back, not
/// be starved by the first.
#[tokio::test]
async fn test_a_request_that_times_out_does_not_starve_the_next_one() {
    let server = MockServer::start(Behaviour::IgnoreFirstChannelRequest).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    // Let the helper's internal timeout elapse rather than cancelling it.
    tokio::time::pause();
    let first = client.create_feed_channel("AUTO").await;
    tokio::time::resume();
    assert!(
        matches!(first, Err(DXLinkError::Timeout(_))),
        "the first request should time out, got: {first:?}"
    );
    assert_eq!(
        client.pending_response_count(),
        0,
        "the timed-out registration was left behind"
    );

    // The server answers from here on; the response must reach this caller.
    let channel = client
        .create_feed_channel("AUTO")
        .await
        .expect("the second request should be answered");
    client
        .setup_feed(channel, &[EventType::Quote])
        .await
        .expect("the channel the second request opened should be usable");

    client.disconnect().await.expect("failed to disconnect");
}

/// A failed send is the third cleanup path: the registration goes in before the
/// send, so a send that errors must not leave it behind.
#[tokio::test]
async fn test_a_failed_send_leaves_no_pending_request() {
    let server = MockServer::start(Behaviour::CloseAfterHandshake).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    // The server hung up right after AUTH, so this send cannot land.
    let mut last = Ok(0);
    for _ in 0..5 {
        last = client.create_feed_channel("AUTO").await;
        if last.is_err() {
            break;
        }
    }
    assert!(
        last.is_err(),
        "the send should fail against a closed socket"
    );

    assert_eq!(
        client.pending_response_count(),
        0,
        "a failed send left a registration behind"
    );
}

// --- Shutdown ordering (issue #8) ------------------------------------------

/// Disconnect used to abort the reader before closing channels, so every
/// close_channel waited out its full timeout for a reply nobody could route:
/// five seconds per open channel. With three channels that is fifteen seconds.
#[tokio::test]
async fn test_disconnect_closes_channels_without_waiting_out_timeouts() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    let mut channels = Vec::new();
    for _ in 0..3 {
        channels.push(
            client
                .create_feed_channel("AUTO")
                .await
                .expect("failed to create feed channel"),
        );
    }

    let started = std::time::Instant::now();
    client.disconnect().await.expect("failed to disconnect");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "disconnect took {elapsed:?}, it waited out per-channel timeouts"
    );

    // Every channel was actually cancelled on the wire, not just forgotten.
    let cancels = server
        .received()
        .iter()
        .filter(|m| m["type"] == "CHANNEL_CANCEL")
        .count();
    assert_eq!(cancels, channels.len(), "not every channel was cancelled");
}

/// A second disconnect must be a no-op, and the client must not claim to still
/// have the channels of a session that is gone.
#[tokio::test]
async fn test_disconnect_is_idempotent_and_clears_session_state() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    client.disconnect().await.expect("first disconnect failed");
    client.disconnect().await.expect("second disconnect failed");

    assert_eq!(
        client.pending_response_count(),
        0,
        "pending responses survived the disconnect"
    );

    // The channel belonged to a connection that no longer exists.
    let result = client.setup_feed(channel_id, &[EventType::Quote]).await;
    assert!(
        result.is_err(),
        "a channel from the closed session was still usable"
    );
}

/// A client must be usable again after disconnecting. The stream flag and the
/// disconnect reason both belonged to the session that ended, and keeping them
/// made the next connect fail or report a healthy session as dead.
#[tokio::test]
async fn test_a_client_can_reconnect_after_disconnecting() {
    let first = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&first.url(), "test-token");

    client.connect().await.expect("first connect failed");
    client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client.disconnect().await.expect("failed to disconnect");

    // A deliberate disconnect is not a failure and must not read as one.
    assert!(
        client.disconnect_reason().is_none(),
        "a deliberate disconnect reported a failure reason"
    );

    let second = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&second.url(), "test-token");
    let _stream = client
        .connect()
        .await
        .expect("a fresh client should connect");
    client
        .create_feed_channel("AUTO")
        .await
        .expect("the new session should be usable");
    client.disconnect().await.expect("failed to disconnect");
}

/// A plain Quote subscription, for the layout tests below.
fn quote_subscription(symbol: &str) -> FeedSubscription {
    FeedSubscription {
        event_type: "Quote".to_string(),
        symbol: symbol.to_string(),
        from_time: None,
        source: None,
    }
}

// --- FEED_CONFIG validation (issue #12) ------------------------------------

/// A server that reorders the field list is **decoded against that order**,
/// not refused.
///
/// This used to be a rejection, which was the right answer while the decoder
/// read by position. It reads by name now, so the order the server picks is
/// simply the order it is read in, and refusing would be turning a
/// non-problem into a dead channel.
#[tokio::test]
async fn test_setup_feed_adopts_a_reordered_field_layout() {
    let server = MockServer::start(Behaviour::ReorderedFeedConfig).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut stream = client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("a reordered layout is decodable, so it must be accepted");

    client
        .subscribe(channel_id, vec![quote_subscription("AAPL")])
        .await
        .expect("failed to subscribe");

    let event = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("no event arrived")
        .expect("the stream closed");

    // The whole point: the values land in the right fields even though the
    // server put the columns somewhere else. Reading by position would have
    // put the symbol in event_type here.
    match event {
        MarketEvent::Quote(quote) => {
            assert_eq!(quote.event_type, "Quote");
            assert_eq!(quote.event_symbol, "AAPL");
            assert_eq!(quote.bid_price, expected::BID_PRICE);
            assert_eq!(quote.ask_price, expected::ASK_PRICE);
            assert_eq!(quote.bid_size, expected::BID_SIZE);
            assert_eq!(quote.ask_size, expected::ASK_SIZE);
        }
        other => panic!("expected a quote, got {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// This client decodes COMPACT rows and nothing else.
#[tokio::test]
async fn test_setup_feed_rejects_a_format_it_cannot_decode() {
    let server = MockServer::start(Behaviour::NonCompactFeedConfig).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    match client.setup_feed(channel_id, &[EventType::Quote]).await {
        Err(DXLinkError::Protocol(msg)) => {
            assert!(msg.contains("COMPACT"), "unclear error: {msg}");
            assert!(
                msg.contains("FULL"),
                "the negotiated format is missing: {msg}"
            );
        }
        other => panic!("a non-COMPACT format must be rejected, got: {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// A reconfiguration mid-session is adopted, and the channel keeps working.
#[tokio::test]
async fn test_a_reconfiguration_is_adopted_and_the_channel_keeps_working() {
    let server = MockServer::start(Behaviour::ReorderedFeedConfigOnSecondSetup).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut stream = client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("the first setup should succeed");

    // The second reply comes back with the columns in a different order, and
    // that is now something to follow rather than refuse.
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("a reconfiguration this client can decode must be accepted");

    client
        .subscribe(channel_id, vec![quote_subscription("AAPL")])
        .await
        .expect("the channel should still be usable after a reconfiguration");

    let event = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("no event arrived after the reconfiguration")
        .expect("the stream closed");

    match event {
        MarketEvent::Quote(quote) => {
            // Decoded against the *new* layout: the old one would have swapped
            // these two.
            assert_eq!(quote.event_type, "Quote");
            assert_eq!(quote.event_symbol, "AAPL");
            assert_eq!(quote.bid_price, expected::BID_PRICE);
        }
        other => panic!("expected a quote, got {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}

// --- Fallible event type parsing (issue #21) -------------------------------

/// A misspelled event type used to go out on the wire verbatim while being
/// recorded locally as Quote, so the client believed in a subscription it had
/// never made and the server had never acknowledged.
#[tokio::test]
async fn test_subscribing_with_an_unknown_event_type_is_rejected() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");

    let result = client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Qutoe".to_string(),
                symbol: "AAPL".to_string(),
                from_time: None,
                source: None,
            }],
        )
        .await;

    match result {
        Err(DXLinkError::Protocol(msg)) => {
            assert!(
                msg.contains("Qutoe"),
                "the offending name is missing: {msg}"
            );
        }
        other => panic!("a misspelled event type must be rejected, got: {other:?}"),
    }

    // And nothing reached the wire.
    let sent = server
        .received()
        .iter()
        .filter(|m| m["type"] == "FEED_SUBSCRIPTION")
        .count();
    assert_eq!(sent, 0, "a rejected subscription still went out");

    client.disconnect().await.expect("failed to disconnect");
}

/// One bad entry rejects the batch: sending half and recording the other half
/// leaves the client and the server disagreeing about what is subscribed.
#[tokio::test]
async fn test_a_batch_with_one_bad_type_is_rejected_whole() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");

    let good = FeedSubscription {
        event_type: "Quote".to_string(),
        symbol: "AAPL".to_string(),
        from_time: None,
        source: None,
    };
    let bad = FeedSubscription {
        event_type: "Nonsense".to_string(),
        symbol: "MSFT".to_string(),
        from_time: None,
        source: None,
    };

    assert!(client.subscribe(channel_id, vec![good, bad]).await.is_err());
    assert_eq!(
        server
            .received()
            .iter()
            .filter(|m| m["type"] == "FEED_SUBSCRIPTION")
            .count(),
        0,
        "a partially valid batch still went out"
    );

    client.disconnect().await.expect("failed to disconnect");
}

// --- No silent empty streams (issue #30) -----------------------------------

/// Configuring a type this client cannot decode used to succeed: the wildcard
/// asked for two fields, the server agreed, the subscription was accepted, and
/// then nothing ever arrived. An empty stream looks exactly like a quiet market.
#[tokio::test]
async fn test_setup_feed_refuses_a_type_it_cannot_decode() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    match client.setup_feed(channel_id, &[EventType::Order]).await {
        Err(DXLinkError::Protocol(msg)) => {
            assert!(msg.contains("Order"), "the type is missing: {msg}");
            // The error has to say what does work, or it is a dead end.
            assert!(msg.contains("Quote"), "the usable types are missing: {msg}");
        }
        other => panic!("an undecodable type must be refused, got: {other:?}"),
    }

    // Nothing was configured on the wire either.
    assert_eq!(
        server
            .received()
            .iter()
            .filter(|m| m["type"] == "FEED_SETUP")
            .count(),
        0,
        "a refused setup still went out"
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// The same applies to subscribing, which is the other way to end up waiting
/// for events that cannot arrive.
#[tokio::test]
async fn test_subscribe_refuses_a_type_it_cannot_decode() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");

    let result = client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Order".to_string(),
                symbol: "AAPL".to_string(),
                from_time: None,
                source: None,
            }],
        )
        .await;

    match result {
        Err(DXLinkError::Protocol(msg)) => {
            assert!(msg.contains("Order"), "the type is missing: {msg}");
            assert!(msg.contains("AAPL"), "the symbol is missing: {msg}");
        }
        other => panic!("an undecodable subscription must be refused, got: {other:?}"),
    }

    // Returning Err while still sending would be the same bug wearing an error.
    assert_eq!(
        server
            .received()
            .iter()
            .filter(|m| m["type"] == "FEED_SUBSCRIPTION")
            .count(),
        0,
        "a refused subscription still went out"
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// Decodable is not enough: the layout must be one this channel negotiated.
/// Subscribing Trade on a Quote-configured channel used to pass, and the
/// reader's channel-level gate then decoded Trade rows against a layout that
/// was never agreed.
#[tokio::test]
async fn test_subscribe_refuses_a_type_the_channel_was_not_configured_for() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");

    let result = client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Trade".to_string(),
                symbol: "AAPL".to_string(),
                from_time: None,
                source: None,
            }],
        )
        .await;

    match result {
        Err(DXLinkError::Protocol(msg)) => {
            assert!(msg.contains("Trade"), "the type is missing: {msg}");
            assert!(msg.contains("setup_feed"), "no way forward given: {msg}");
        }
        other => panic!("Trade on a Quote channel must be refused, got: {other:?}"),
    }

    assert_eq!(
        server
            .received()
            .iter()
            .filter(|m| m["type"] == "FEED_SUBSCRIPTION")
            .count(),
        0,
        "a refused subscription still went out"
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// An empty configuration subscribes to nothing and can never deliver.
#[tokio::test]
async fn test_setup_feed_refuses_an_empty_event_type_list() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    assert!(
        client.setup_feed(channel_id, &[]).await.is_err(),
        "an empty event type list must be refused"
    );
    assert_eq!(
        server
            .received()
            .iter()
            .filter(|m| m["type"] == "FEED_SETUP")
            .count(),
        0,
        "a refused setup still went out"
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// Issue #63, reproduced offline: the server serves **fewer** fields than were
/// asked for.
///
/// The dxFeed demo drops `VWAP` from `Candle`. Reading by position against the
/// requested 18 columns meant the 17 that arrived were not a whole number of
/// rows, so the whole batch was discarded and `Candle` delivered nothing at
/// all. Reading by name, the bar decodes and the missing field reads as `NaN`.
#[tokio::test]
async fn test_a_trimmed_layout_still_decodes() {
    let server = MockServer::start(Behaviour::TrimsCandleVwap).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut stream = client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    client
        .setup_feed(channel_id, &[EventType::Candle])
        .await
        .expect("a server serving a subset is still decodable");

    client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Candle".to_string(),
                symbol: "AAPL{=5m}".to_string(),
                from_time: None,
                source: None,
            }],
        )
        .await
        .expect("failed to subscribe");

    let event = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("no bar arrived, which is exactly the bug")
        .expect("the stream closed");

    match event {
        MarketEvent::Candle(candle) => {
            // Everything after the missing column still lands correctly, which
            // is what a positional read could not do.
            assert_eq!(candle.event_symbol, "AAPL{=5m}");
            assert_eq!(candle.volume, expected::VOLUME);
            assert_eq!(candle.bid_volume, expected::BID_VOLUME);
            assert_eq!(candle.ask_volume, expected::ASK_VOLUME);
            assert_eq!(candle.imp_volatility, expected::IMP_VOLATILITY);
            assert_eq!(candle.open_interest, expected::OPEN_INTEREST);
            // And the one the server does not serve is absent, not wrong.
            assert!(
                candle.vwap.is_nan(),
                "a field the server did not send must not come back as a number: {}",
                candle.vwap
            );
        }
        other => panic!("expected a candle, got {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// The other half of issue #63: the real layout arrives **after** the
/// subscription, not as the reply to `FEED_SETUP`.
///
/// The demo server acknowledges the setup with no field list at all, then
/// reports what it will really serve once there is something to serve. That
/// second config used to invalidate the channel, so every row that followed was
/// dropped. It is adopted now.
#[tokio::test]
async fn test_a_late_config_is_adopted_rather_than_fatal() {
    let server = MockServer::start(Behaviour::LateFeedConfig).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut stream = client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("an acknowledgement with no field list is agreement, not a refusal");

    client
        .subscribe(channel_id, vec![quote_subscription("AAPL")])
        .await
        .expect("failed to subscribe");

    let event = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("no event arrived after the late config")
        .expect("the stream closed");

    match event {
        MarketEvent::Quote(quote) => {
            assert_eq!(quote.event_symbol, "AAPL");
            assert_eq!(quote.bid_price, expected::BID_PRICE);
            assert_eq!(quote.ask_price, expected::ASK_PRICE);
        }
        other => panic!("expected a quote, got {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// A mid-session layout this client cannot read has to **stop** the channel,
/// not be quietly ignored.
///
/// Ignoring it leaves the old layout installed while the server sends rows in
/// the new one. When the two happen to have the same column count — which is
/// exactly the case constructed here — that decodes into wrong values instead
/// of into an error, which is the failure mode this whole area exists to
/// prevent.
#[tokio::test]
async fn test_an_unreadable_reconfiguration_stops_the_channel() {
    let server = MockServer::start(Behaviour::UnusableConfigMidSession).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut stream = client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("the setup itself is fine");

    client
        .subscribe(channel_id, vec![quote_subscription("AAPL")])
        .await
        .expect("failed to subscribe");

    // Nothing may be delivered.
    let delivered = tokio::time::timeout(Duration::from_secs(2), stream.recv()).await;
    assert!(
        delivered.is_err(),
        "a layout this client cannot read must stop delivery, got {delivered:?}"
    );

    // And the channel is stopped, not merely producing an error per batch. This
    // is the part that distinguishes failing closed from ignoring the config:
    // with the layout gone, the refusal comes before anything is sent rather
    // than after every row that arrives.
    assert!(
        client
            .subscribe(channel_id, vec![quote_subscription("MSFT")])
            .await
            .is_err(),
        "a channel whose layout the server moved past must stop accepting work"
    );

    client.disconnect().await.expect("failed to disconnect");
}
