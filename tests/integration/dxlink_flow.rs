//! End-to-end tests that drive the public API exactly as `examples/miscellaneous/src/bin/basic.rs`
//! does: connect, open a feed channel, configure it, register a callback, take
//! the event stream, subscribe, and consume market events.
//!
//! Unlike the older suites these assert the *values* that reach the consumer, so
//! they fail if the COMPACT column order in `setup_feed` and the stride in
//! `parse_compact_data` ever drift apart — the one defect in this codebase that
//! produces wrong numbers instead of an error.
//!
//! `control_frames` mode interleaves WebSocket Pings with the protocol traffic,
//! which is the end-to-end regression test for issue #4: before the fix, the
//! first Ping aborted the handshake and no session was ever established.

use dxlink::{DXLinkClient, EventType, FeedSubscription, MarketEvent};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A DXLink server that speaks enough of the protocol to run a full session,
/// and can interleave WebSocket control frames with every response.
mod flow_server {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    pub struct FlowServer {
        pub address: SocketAddr,
        received: Arc<Mutex<Vec<Value>>>,
    }

    impl FlowServer {
        /// Every message the client sent, in order.
        pub fn received(&self) -> Vec<Value> {
            self.received
                .lock()
                .expect("received lock poisoned")
                .clone()
        }

        pub fn url(&self) -> String {
            format!("ws://{}", self.address)
        }
    }

    /// What the server does besides answering the protocol.
    #[derive(Clone, Copy, PartialEq)]
    pub enum Behaviour {
        /// Plain text responses only.
        Plain,
        /// Send a Ping before, and a Pong after, every response. Control frames
        /// are ordinary traffic and must never break a session.
        ControlFrames,
        /// Complete the handshake, then hang up once the feed is subscribed.
        CloseAfterSubscribe,
    }

    /// Column order for COMPACT rows. This MUST match the field list that
    /// `setup_feed` requests for each event type — the server echoes back the
    /// fields in the order they were asked for.
    fn quote_row(symbol: &str) -> Value {
        // eventType, eventSymbol, bidPrice, askPrice, bidSize, askSize
        json!(["Quote", symbol, 150.25, 150.5, 100.0, 150.0])
    }

    fn trade_row(symbol: &str) -> Value {
        // eventType, eventSymbol, price, size, dayVolume
        json!(["Trade", symbol, 151.25, 75.0, 10_000_000.0])
    }

    pub async fn start(behaviour: Behaviour) -> FlowServer {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind flow server");
        let address = listener.local_addr().expect("failed to read local addr");

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_task = received.clone();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("failed to accept");
            let mut ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("failed to handshake");

            // One task owns the whole socket, so requests and responses stay
            // ordered and there is no sink to share.
            while let Some(Ok(message)) = ws.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };

                received_task
                    .lock()
                    .expect("received lock poisoned")
                    .push(value.clone());

                let channel = value["channel"].as_u64().unwrap_or(0);
                let mut responses: Vec<Value> = Vec::new();
                let mut close_after = false;

                match value["type"].as_str().unwrap_or("") {
                    "SETUP" => {
                        responses.push(json!({
                            "channel": channel,
                            "type": "SETUP",
                            "version": "1.0.0",
                            "keepaliveTimeout": 60,
                            "acceptKeepaliveTimeout": 60
                        }));
                        responses.push(json!({
                            "channel": 0,
                            "type": "AUTH_STATE",
                            "state": "UNAUTHORIZED"
                        }));
                    }
                    "AUTH" => responses.push(json!({
                        "channel": 0,
                        "type": "AUTH_STATE",
                        "state": "AUTHORIZED",
                        "userId": "test-user"
                    })),
                    "CHANNEL_REQUEST" => responses.push(json!({
                        "channel": channel,
                        "type": "CHANNEL_OPENED",
                        "service": "FEED",
                        "parameters": {}
                    })),
                    "FEED_SETUP" => responses.push(json!({
                        "channel": channel,
                        "type": "FEED_CONFIG",
                        "aggregationPeriod": 0.1,
                        "dataFormat": "COMPACT"
                    })),
                    "FEED_SUBSCRIPTION" => {
                        if let Some(add) = value.get("add").and_then(|a| a.as_array()) {
                            for sub in add {
                                let symbol = sub["symbol"].as_str().unwrap_or("");
                                let row = match sub["type"].as_str().unwrap_or("") {
                                    "Quote" => quote_row(symbol),
                                    "Trade" => trade_row(symbol),
                                    _ => continue,
                                };
                                let event_type = sub["type"].as_str().unwrap_or("");
                                responses.push(json!({
                                    "channel": channel,
                                    "type": "FEED_DATA",
                                    "data": [event_type, row]
                                }));
                            }
                        }
                        close_after = behaviour == Behaviour::CloseAfterSubscribe;
                    }
                    // KEEPALIVE and anything else needs no reply.
                    _ => {}
                }

                for response in responses {
                    if behaviour == Behaviour::ControlFrames {
                        ws.send(Message::Ping(vec![7].into()))
                            .await
                            .expect("failed to send ping");
                    }

                    ws.send(Message::Text(response.to_string().into()))
                        .await
                        .expect("failed to send response");

                    if behaviour == Behaviour::ControlFrames {
                        ws.send(Message::Pong(Vec::new().into()))
                            .await
                            .expect("failed to send pong");
                    }
                }

                if close_after {
                    let _ = ws.send(Message::Close(None)).await;
                    return;
                }
            }
        });

        FlowServer { address, received }
    }
}

use flow_server::Behaviour;

/// Collects events off the stream until `wanted` have arrived or time runs out.
async fn collect_events(
    stream: &mut tokio::sync::mpsc::Receiver<MarketEvent>,
    wanted: usize,
) -> Vec<MarketEvent> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

    while events.len() < wanted {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.recv()).await {
            Ok(Some(event)) => events.push(event),
            // Channel closed or deadline hit; return what we have and let the
            // caller's assertion report the shortfall.
            Ok(None) | Err(_) => break,
        }
    }

    events
}

/// Runs the same sequence as the `basic` example and checks the market data that
/// comes out the other end, field by field.
async fn run_full_session(behaviour: Behaviour) {
    let server = flow_server::start(behaviour).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut event_stream = client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    client
        .setup_feed(channel_id, &[EventType::Quote, EventType::Trade])
        .await
        .expect("failed to set up feed");

    // A callback scoped to one symbol, exactly as the example registers it.
    let callback_events = Arc::new(Mutex::new(Vec::new()));
    let callback_sink = callback_events.clone();
    client.on_event("AAPL", move |event| {
        callback_sink
            .lock()
            .expect("callback lock poisoned")
            .push(event);
    });

    client
        .subscribe(
            channel_id,
            vec![
                FeedSubscription {
                    event_type: "Quote".to_string(),
                    symbol: "AAPL".to_string(),
                    from_time: None,
                    source: None,
                },
                FeedSubscription {
                    event_type: "Trade".to_string(),
                    symbol: "AAPL".to_string(),
                    from_time: None,
                    source: None,
                },
            ],
        )
        .await
        .expect("failed to subscribe");

    let events = collect_events(&mut event_stream, 2).await;
    assert_eq!(
        events.len(),
        2,
        "expected a Quote and a Trade to reach the stream, got {events:?}"
    );

    let quote = events
        .iter()
        .find_map(|e| match e {
            MarketEvent::Quote(q) => Some(q),
            _ => None,
        })
        .expect("no Quote event reached the consumer");

    // The symbol is the assertion that catches a COMPACT column-order drift: if
    // eventType and eventSymbol swap, this reads "Quote" instead of "AAPL".
    assert_eq!(quote.event_symbol, "AAPL");
    assert_eq!(quote.event_type, "Quote");
    assert_eq!(quote.bid_price, 150.25);
    assert_eq!(quote.ask_price, 150.5);
    assert_eq!(quote.bid_size, 100.0);
    assert_eq!(quote.ask_size, 150.0);

    let trade = events
        .iter()
        .find_map(|e| match e {
            MarketEvent::Trade(t) => Some(t),
            _ => None,
        })
        .expect("no Trade event reached the consumer");

    assert_eq!(trade.event_symbol, "AAPL");
    assert_eq!(trade.price, 151.25);
    assert_eq!(trade.size, 75.0);
    assert_eq!(trade.day_volume, 10_000_000.0);

    // The callback path is separate from the stream path; both must fire.
    // Copy out of the lock in its own scope so no guard reaches the await below.
    let delivered: Vec<MarketEvent> = {
        let events = callback_events.lock().expect("callback lock poisoned");
        events.clone()
    };
    assert_eq!(
        delivered.len(),
        2,
        "callback for AAPL should have received both events, got {delivered:?}"
    );

    client.disconnect().await.expect("failed to disconnect");
}

#[tokio::test]
async fn test_full_session_delivers_market_data() {
    run_full_session(Behaviour::Plain).await;
}

/// End-to-end regression for issue #4. Before the fix the first Ping aborted the
/// handshake, so this never got as far as a quote.
#[tokio::test]
async fn test_full_session_survives_interleaved_control_frames() {
    run_full_session(Behaviour::ControlFrames).await;
}

/// A callback registered for one symbol must not see another symbol's events.
#[tokio::test]
async fn test_callback_is_scoped_to_its_symbol() {
    let server = flow_server::start(Behaviour::Plain).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut event_stream = client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");

    let msft_events = Arc::new(Mutex::new(Vec::new()));
    let msft_sink = msft_events.clone();
    client.on_event("MSFT", move |event| {
        msft_sink.lock().expect("lock poisoned").push(event);
    });

    client
        .subscribe(
            channel_id,
            vec![
                FeedSubscription {
                    event_type: "Quote".to_string(),
                    symbol: "AAPL".to_string(),
                    from_time: None,
                    source: None,
                },
                FeedSubscription {
                    event_type: "Quote".to_string(),
                    symbol: "MSFT".to_string(),
                    from_time: None,
                    source: None,
                },
            ],
        )
        .await
        .expect("failed to subscribe");

    let events = collect_events(&mut event_stream, 2).await;
    assert_eq!(events.len(), 2, "both symbols should reach the stream");

    let symbols: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            MarketEvent::Quote(q) => Some(q.event_symbol.as_str()),
            _ => None,
        })
        .collect();
    assert!(symbols.contains(&"AAPL"), "AAPL missing from {symbols:?}");
    assert!(symbols.contains(&"MSFT"), "MSFT missing from {symbols:?}");

    // Copy out of the lock in its own scope so no guard reaches the await below.
    let delivered: Vec<MarketEvent> = {
        let events = msft_events.lock().expect("lock poisoned");
        events.clone()
    };
    assert_eq!(delivered.len(), 1, "MSFT callback should fire exactly once");
    match &delivered[0] {
        MarketEvent::Quote(q) => assert_eq!(q.event_symbol, "MSFT"),
        other => panic!("MSFT callback received the wrong event: {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// The client must survive the server hanging up mid-session: no panic, no hang,
/// and `disconnect` still succeeds.
#[tokio::test]
async fn test_session_survives_server_close() {
    let server = flow_server::start(Behaviour::CloseAfterSubscribe).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut event_stream = client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");

    client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Quote".to_string(),
                symbol: "AAPL".to_string(),
                from_time: None,
                source: None,
            }],
        )
        .await
        .expect("failed to subscribe");

    // The quote is sent before the close, so it must still arrive.
    let events = collect_events(&mut event_stream, 1).await;
    assert_eq!(
        events.len(),
        1,
        "the event sent before the close should still be delivered"
    );

    // The server hung up; tearing down must not hang or panic.
    tokio::time::timeout(Duration::from_secs(5), client.disconnect())
        .await
        .expect("disconnect hung after the server closed the connection")
        .expect("disconnect failed after the server closed the connection");
}

/// The protocol exchange itself: the client must send the messages a real server
/// expects, in order, with the token it was given.
#[tokio::test]
async fn test_session_sends_the_expected_protocol_messages() {
    let server = flow_server::start(Behaviour::Plain).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut event_stream = client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");
    client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Quote".to_string(),
                symbol: "AAPL".to_string(),
                from_time: None,
                source: None,
            }],
        )
        .await
        .expect("failed to subscribe");

    // Wait for the event so the subscription is known to have been processed.
    assert_eq!(collect_events(&mut event_stream, 1).await.len(), 1);

    let received = server.received();
    let types: Vec<&str> = received
        .iter()
        .filter_map(|m| m["type"].as_str())
        .filter(|t| *t != "KEEPALIVE")
        .collect();
    assert_eq!(
        types,
        vec![
            "SETUP",
            "AUTH",
            "CHANNEL_REQUEST",
            "FEED_SETUP",
            "FEED_SUBSCRIPTION"
        ],
        "unexpected protocol sequence"
    );

    let auth = received
        .iter()
        .find(|m| m["type"] == "AUTH")
        .expect("no AUTH message");
    assert_eq!(auth["token"], "test-token");

    // FEED_SETUP is the half of the COMPACT contract that pins the column order;
    // if this list changes, parse_compact_data must change with it.
    let feed_setup = received
        .iter()
        .find(|m| m["type"] == "FEED_SETUP")
        .expect("no FEED_SETUP message");
    assert_eq!(feed_setup["acceptDataFormat"], "COMPACT");
    let quote_fields = feed_setup["acceptEventFields"]["Quote"]
        .as_array()
        .expect("no Quote field list in FEED_SETUP");
    let quote_fields: Vec<&str> = quote_fields.iter().filter_map(|f| f.as_str()).collect();
    assert_eq!(
        quote_fields,
        vec![
            "eventType",
            "eventSymbol",
            "bidPrice",
            "askPrice",
            "bidSize",
            "askSize"
        ],
        "Quote column order changed; parse_compact_data must match it"
    );

    client.disconnect().await.expect("failed to disconnect");
}
