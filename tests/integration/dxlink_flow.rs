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

use crate::fixture::{Behaviour, MockServer, expected};

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
    let server = MockServer::start(behaviour).await;

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
    assert_eq!(quote.bid_price, expected::BID_PRICE);
    assert_eq!(quote.ask_price, expected::ASK_PRICE);
    assert_eq!(quote.bid_size, expected::BID_SIZE);
    assert_eq!(quote.ask_size, expected::ASK_SIZE);

    let trade = events
        .iter()
        .find_map(|e| match e {
            MarketEvent::Trade(t) => Some(t),
            _ => None,
        })
        .expect("no Trade event reached the consumer");

    assert_eq!(trade.event_symbol, "AAPL");
    assert_eq!(trade.price, expected::PRICE);
    assert_eq!(trade.size, expected::SIZE);
    assert_eq!(trade.day_volume, expected::DAY_VOLUME);

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
    run_full_session(Behaviour::Normal).await;
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
    let server = MockServer::start(Behaviour::Normal).await;

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
    let server = MockServer::start(Behaviour::CloseAfterSubscribe).await;

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
    let server = MockServer::start(Behaviour::Normal).await;

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

// --- Delivery isolation (issue #10) ----------------------------------------

/// A slow callback must not stop socket reads. It used to run on the same task
/// that routes protocol responses, so an unrelated channel operation timed out
/// because somebody's callback was busy.
// Multi-threaded on purpose: the point is that a blocking callback does not
// stall protocol work, and the default single-threaded test runtime would make
// the result depend on the executor rather than on the decoupling.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_slow_callback_does_not_block_protocol_operations() {
    let server = MockServer::start(Behaviour::Normal).await;

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

    // Blocks the delivery worker for far longer than a protocol round trip.
    client.on_event("AAPL", |_| {
        std::thread::sleep(Duration::from_millis(1500));
    });

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

    // Give the callback time to be in flight.
    let _ = collect_events(&mut event_stream, 1).await;

    // A protocol operation must still complete promptly.
    let started = std::time::Instant::now();
    let second = tokio::time::timeout(Duration::from_secs(3), client.create_feed_channel("AUTO"))
        .await
        .expect("a channel operation was blocked by a slow callback");
    assert!(second.is_ok(), "channel operation failed: {second:?}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "channel operation took {:?}, it was waiting on the callback",
        started.elapsed()
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// A panicking callback used to take the whole task down, and with it every
/// protocol response.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_panicking_callback_does_not_kill_the_session() {
    let server = MockServer::start(Behaviour::Normal).await;

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

    client.on_event("AAPL", |_| panic!("callback blew up"));

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

    // The event still reaches the stream even though the callback panicked.
    let events = collect_events(&mut event_stream, 1).await;
    assert_eq!(events.len(), 1, "the stream lost the event to the panic");

    // And the session is still usable.
    client
        .create_feed_channel("AUTO")
        .await
        .expect("the session died with the callback");

    client.disconnect().await.expect("failed to disconnect");
}

// --- Losing the connection is observable (issue #6) ------------------------

/// The stream closing is the signal that the session is over, and
/// disconnect_reason says why. Before this the consumer waited forever on a
/// stream that would never produce again, with only a log line to go on.
#[tokio::test]
async fn test_a_lost_connection_closes_the_stream_and_reports_why() {
    let server = MockServer::start(Behaviour::CloseAfterSubscribe).await;

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

    assert!(
        client.disconnect_reason().is_none(),
        "a healthy session should have no disconnect reason"
    );

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

    // Drain until the channel closes. Reaching None at all is the point: it used
    // to block forever because the client held a sender of its own.
    let drained = tokio::time::timeout(Duration::from_secs(10), async {
        while event_stream.recv().await.is_some() {}
    })
    .await;
    assert!(
        drained.is_ok(),
        "the stream never closed after the server hung up"
    );

    let reason = client
        .disconnect_reason()
        .expect("the session ended without recording why");
    assert!(
        reason.contains("closed") || reason.contains("Connection"),
        "unhelpful disconnect reason: {reason}"
    );
}

/// Issue #25 asks for two Summary rows reaching both delivery paths without
/// column drift. Two symbols, so a stride error shows up as the second event
/// carrying the wrong symbol rather than as a plausible number.
#[tokio::test]
async fn test_summaries_reach_the_stream_and_the_callback() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut event_stream = client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Summary])
        .await
        .expect("failed to set up feed");

    let delivered = Arc::new(Mutex::new(Vec::new()));
    let sink = delivered.clone();
    client.on_event("AAPL", move |event| {
        sink.lock().expect("callback lock poisoned").push(event);
    });

    client
        .subscribe(
            channel_id,
            vec![
                FeedSubscription {
                    event_type: "Summary".to_string(),
                    symbol: "AAPL".to_string(),
                    from_time: None,
                    source: None,
                },
                FeedSubscription {
                    event_type: "Summary".to_string(),
                    symbol: "MSFT".to_string(),
                    from_time: None,
                    source: None,
                },
            ],
        )
        .await
        .expect("failed to subscribe");

    let events = collect_events(&mut event_stream, 2).await;
    assert_eq!(events.len(), 2, "expected two summaries, got {events:?}");

    let symbols: Vec<&str> = events
        .iter()
        .map(|event| match event {
            MarketEvent::Summary(summary) => summary.event_symbol.as_str(),
            other => panic!("expected a summary, got {other:?}"),
        })
        .collect();
    assert!(symbols.contains(&"AAPL"), "AAPL is missing: {symbols:?}");
    assert!(symbols.contains(&"MSFT"), "MSFT is missing: {symbols:?}");

    let summary = events
        .iter()
        .find_map(|event| match event {
            MarketEvent::Summary(summary) if summary.event_symbol == "AAPL" => Some(summary),
            _ => None,
        })
        .expect("no Summary for AAPL reached the stream");

    assert_eq!(summary.event_type, "Summary");
    assert_eq!(summary.event_time, expected::EVENT_TIME);
    assert_eq!(summary.day_id, expected::DAY_ID);
    assert_eq!(summary.day_open_price, expected::DAY_OPEN_PRICE);
    assert_eq!(summary.day_high_price, expected::DAY_HIGH_PRICE);
    assert_eq!(summary.day_low_price, expected::DAY_LOW_PRICE);
    assert_eq!(summary.day_close_price, expected::DAY_CLOSE_PRICE);
    assert_eq!(summary.day_close_price_type, expected::DAY_CLOSE_PRICE_TYPE);
    assert_eq!(summary.prev_day_id, expected::PREV_DAY_ID);
    assert_eq!(summary.prev_day_close_price, expected::PREV_DAY_CLOSE_PRICE);
    assert_eq!(
        summary.prev_day_close_price_type,
        expected::PREV_DAY_CLOSE_PRICE_TYPE
    );
    assert_eq!(summary.prev_day_volume, expected::PREV_DAY_VOLUME);
    assert_eq!(summary.open_interest, expected::OPEN_INTEREST);

    // The callback routes through symbol_of, which needed a new arm for this
    // variant, and it is scoped to AAPL so MSFT must not appear.
    let seen: Vec<MarketEvent> = {
        let events = delivered.lock().expect("callback lock poisoned");
        events.clone()
    };
    match seen.as_slice() {
        [MarketEvent::Summary(summary)] => assert_eq!(summary.event_symbol, "AAPL"),
        other => panic!("the callback for AAPL should have received one summary, got {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// Issue #26 asks for two prints reaching both delivery paths with their sale
/// conditions, timestamps, prices and booleans intact.
#[tokio::test]
async fn test_prints_reach_the_stream_and_the_callback() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut event_stream = client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::TimeAndSale])
        .await
        .expect("failed to set up feed");

    let delivered = Arc::new(Mutex::new(Vec::new()));
    let sink = delivered.clone();
    client.on_event("AAPL", move |event| {
        sink.lock().expect("callback lock poisoned").push(event);
    });

    client
        .subscribe(
            channel_id,
            vec![
                FeedSubscription {
                    event_type: "TimeAndSale".to_string(),
                    symbol: "AAPL".to_string(),
                    from_time: None,
                    source: None,
                },
                FeedSubscription {
                    event_type: "TimeAndSale".to_string(),
                    symbol: "MSFT".to_string(),
                    from_time: None,
                    source: None,
                },
            ],
        )
        .await
        .expect("failed to subscribe");

    let events = collect_events(&mut event_stream, 2).await;
    assert_eq!(events.len(), 2, "expected two prints, got {events:?}");

    let symbols: Vec<&str> = events
        .iter()
        .map(|event| match event {
            MarketEvent::TimeAndSale(print) => print.event_symbol.as_str(),
            other => panic!("expected a print, got {other:?}"),
        })
        .collect();
    assert!(symbols.contains(&"AAPL"), "AAPL is missing: {symbols:?}");
    assert!(symbols.contains(&"MSFT"), "MSFT is missing: {symbols:?}");

    let print = events
        .iter()
        .find_map(|event| match event {
            MarketEvent::TimeAndSale(print) if print.event_symbol == "AAPL" => Some(print),
            _ => None,
        })
        .expect("no TimeAndSale for AAPL reached the stream");

    assert_eq!(print.event_type, "TimeAndSale");
    assert_eq!(print.event_time, expected::EVENT_TIME);
    assert_eq!(print.event_flags, expected::EVENT_FLAGS);
    assert_eq!(print.index, expected::INDEX);
    assert_eq!(print.time, expected::TIME);
    assert_eq!(print.time_nano_part, expected::TIME_NANO_PART);
    assert_eq!(print.sequence, expected::SEQUENCE);
    assert_eq!(print.exchange_code, expected::EXCHANGE_CODE);
    assert_eq!(print.price, expected::PRICE);
    assert_eq!(print.size, expected::SIZE);
    assert_eq!(print.bid_price, expected::BID_PRICE);
    assert_eq!(print.ask_price, expected::ASK_PRICE);
    assert_eq!(
        print.exchange_sale_conditions,
        expected::EXCHANGE_SALE_CONDITIONS
    );
    assert_eq!(print.trade_through_exempt, expected::TRADE_THROUGH_EXEMPT);
    assert_eq!(print.aggressor_side, expected::AGGRESSOR_SIDE);
    assert_eq!(print.spread_leg, expected::SPREAD_LEG);
    assert_eq!(
        print.extended_trading_hours,
        expected::EXTENDED_TRADING_HOURS
    );
    assert_eq!(print.valid_tick, expected::VALID_TICK);
    assert_eq!(print.sale_type, expected::SALE_TYPE);
    assert_eq!(print.buyer, expected::BUYER);
    assert_eq!(print.seller, expected::SELLER);

    // The callback routes through symbol_of, which needed a new arm for this
    // variant, and it is scoped to AAPL so MSFT must not appear.
    let seen: Vec<MarketEvent> = {
        let events = delivered.lock().expect("callback lock poisoned");
        events.clone()
    };
    match seen.as_slice() {
        [MarketEvent::TimeAndSale(print)] => {
            assert_eq!(print.event_symbol, "AAPL");
            // The flags matter most on this path: a consumer acting on prints
            // reads valid_tick before it moves a last price.
            assert_eq!(print.valid_tick, expected::VALID_TICK);
            assert_eq!(print.aggressor_side, expected::AGGRESSOR_SIDE);
        }
        other => panic!("the callback for AAPL should have received one print, got {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// Issue #27 asks for two profiles reaching both delivery paths with all their
/// metadata intact.
#[tokio::test]
async fn test_profiles_reach_the_stream_and_the_callback() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut event_stream = client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Profile])
        .await
        .expect("failed to set up feed");

    let delivered = Arc::new(Mutex::new(Vec::new()));
    let sink = delivered.clone();
    client.on_event("AAPL", move |event| {
        sink.lock().expect("callback lock poisoned").push(event);
    });

    client
        .subscribe(
            channel_id,
            vec![
                FeedSubscription {
                    event_type: "Profile".to_string(),
                    symbol: "AAPL".to_string(),
                    from_time: None,
                    source: None,
                },
                FeedSubscription {
                    event_type: "Profile".to_string(),
                    symbol: "MSFT".to_string(),
                    from_time: None,
                    source: None,
                },
            ],
        )
        .await
        .expect("failed to subscribe");

    let events = collect_events(&mut event_stream, 2).await;
    assert_eq!(events.len(), 2, "expected two profiles, got {events:?}");

    let symbols: Vec<&str> = events
        .iter()
        .map(|event| match event {
            MarketEvent::Profile(profile) => profile.event_symbol.as_str(),
            other => panic!("expected a profile, got {other:?}"),
        })
        .collect();
    assert!(symbols.contains(&"AAPL"), "AAPL is missing: {symbols:?}");
    assert!(symbols.contains(&"MSFT"), "MSFT is missing: {symbols:?}");

    let profile = events
        .iter()
        .find_map(|event| match event {
            MarketEvent::Profile(profile) if profile.event_symbol == "AAPL" => Some(profile),
            _ => None,
        })
        .expect("no Profile for AAPL reached the stream");

    assert_eq!(profile.event_type, "Profile");
    assert_eq!(profile.event_time, expected::EVENT_TIME);
    assert_eq!(profile.description, expected::DESCRIPTION);
    assert_eq!(
        profile.short_sale_restriction,
        expected::SHORT_SALE_RESTRICTION
    );
    assert_eq!(profile.trading_status, expected::TRADING_STATUS);
    assert_eq!(profile.status_reason, expected::STATUS_REASON);
    assert_eq!(profile.halt_start_time, expected::HALT_START_TIME);
    assert_eq!(profile.halt_end_time, expected::HALT_END_TIME);
    assert_eq!(profile.high_limit_price, expected::HIGH_LIMIT_PRICE);
    assert_eq!(profile.low_limit_price, expected::LOW_LIMIT_PRICE);
    assert_eq!(profile.high_52_week_price, expected::HIGH_52_WEEK_PRICE);
    assert_eq!(profile.low_52_week_price, expected::LOW_52_WEEK_PRICE);
    assert_eq!(profile.beta, expected::BETA);
    assert_eq!(profile.earnings_per_share, expected::EARNINGS_PER_SHARE);
    assert_eq!(profile.dividend_frequency, expected::DIVIDEND_FREQUENCY);
    assert_eq!(profile.ex_dividend_amount, expected::EX_DIVIDEND_AMOUNT);
    assert_eq!(profile.ex_dividend_day_id, expected::EX_DIVIDEND_DAY_ID);
    assert_eq!(profile.shares, expected::SHARES);
    assert_eq!(profile.free_float, expected::FREE_FLOAT);

    // The callback routes through symbol_of, which needed a new arm for this
    // variant, and it is scoped to AAPL so MSFT must not appear.
    let seen: Vec<MarketEvent> = {
        let events = delivered.lock().expect("callback lock poisoned");
        events.clone()
    };
    match seen.as_slice() {
        [MarketEvent::Profile(profile)] => {
            assert_eq!(profile.event_symbol, "AAPL");
            // The operational half is what a consumer acts on: a price from a
            // halted instrument is stale by definition.
            assert_eq!(profile.trading_status, expected::TRADING_STATUS);
            assert_eq!(profile.status_reason, expected::STATUS_REASON);
        }
        other => panic!("the callback for AAPL should have received one profile, got {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// Issue #28 asks for two underlyings reaching both delivery paths with their
/// volatility term structure and volumes intact.
#[tokio::test]
async fn test_underlyings_reach_the_stream_and_the_callback() {
    let server = MockServer::start(Behaviour::Normal).await;

    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut event_stream = client.connect().await.expect("failed to connect");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Underlying])
        .await
        .expect("failed to set up feed");

    let delivered = Arc::new(Mutex::new(Vec::new()));
    let sink = delivered.clone();
    client.on_event("SPX", move |event| {
        sink.lock().expect("callback lock poisoned").push(event);
    });

    client
        .subscribe(
            channel_id,
            vec![
                FeedSubscription {
                    event_type: "Underlying".to_string(),
                    symbol: "SPX".to_string(),
                    from_time: None,
                    source: None,
                },
                FeedSubscription {
                    event_type: "Underlying".to_string(),
                    symbol: "NDX".to_string(),
                    from_time: None,
                    source: None,
                },
            ],
        )
        .await
        .expect("failed to subscribe");

    let events = collect_events(&mut event_stream, 2).await;
    assert_eq!(events.len(), 2, "expected two underlyings, got {events:?}");

    let symbols: Vec<&str> = events
        .iter()
        .map(|event| match event {
            MarketEvent::Underlying(surface) => surface.event_symbol.as_str(),
            other => panic!("expected an underlying, got {other:?}"),
        })
        .collect();
    assert!(symbols.contains(&"SPX"), "SPX is missing: {symbols:?}");
    assert!(symbols.contains(&"NDX"), "NDX is missing: {symbols:?}");

    let surface = events
        .iter()
        .find_map(|event| match event {
            MarketEvent::Underlying(surface) if surface.event_symbol == "SPX" => Some(surface),
            _ => None,
        })
        .expect("no Underlying for SPX reached the stream");

    assert_eq!(surface.event_type, "Underlying");
    assert_eq!(surface.event_time, expected::EVENT_TIME);
    assert_eq!(surface.event_flags, expected::EVENT_FLAGS);
    assert_eq!(surface.index, expected::INDEX);
    assert_eq!(surface.time, expected::TIME);
    assert_eq!(surface.sequence, expected::SEQUENCE);
    assert_eq!(surface.volatility, expected::VOLATILITY);
    assert_eq!(surface.front_volatility, expected::FRONT_VOLATILITY);
    assert_eq!(surface.back_volatility, expected::BACK_VOLATILITY);
    assert_eq!(surface.call_volume, expected::CALL_VOLUME);
    assert_eq!(surface.put_volume, expected::PUT_VOLUME);
    assert_eq!(surface.put_call_ratio, expected::PUT_CALL_RATIO);

    // The callback routes through symbol_of, which needed a new arm for this
    // variant, and it is scoped to SPX so NDX must not appear.
    let seen: Vec<MarketEvent> = {
        let events = delivered.lock().expect("callback lock poisoned");
        events.clone()
    };
    match seen.as_slice() {
        [MarketEvent::Underlying(surface)] => {
            assert_eq!(surface.event_symbol, "SPX");
            // The three volatilities are all plausible fractions, so asserting
            // them here is what catches a shift between the two paths.
            assert_eq!(surface.volatility, expected::VOLATILITY);
            assert_eq!(surface.front_volatility, expected::FRONT_VOLATILITY);
            assert_eq!(surface.back_volatility, expected::BACK_VOLATILITY);
        }
        other => panic!("the callback for SPX should have received one underlying, got {other:?}"),
    }

    client.disconnect().await.expect("failed to disconnect");
}
