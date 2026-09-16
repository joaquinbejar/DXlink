//! What the bounded event stream means for a history replay (issue #71).
//!
//! The venue answers a `fromTime` subscription with the whole snapshot in one
//! burst, usually before the consumer's read loop has started, and the last
//! event of that burst is the one carrying `SNAPSHOT_END`. The stream used to
//! hold 100 events and drop the rest at `debug` level, so the consumer got a
//! complete-looking run of bars and never the terminator. These tests hold the
//! stream unread while the burst lands, which is the pattern that broke, and
//! check both that the default buffer now absorbs it and that an undersized
//! one reports exactly what it lost.

use dxlink::{
    ConnectionState, DXLinkClient, EventType, FeedSubscription, MarketEvent, ReconnectPolicy,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::mpsc::Receiver;

use crate::fixture::{Behaviour, HISTORY_BURST_BARS, MockServer, SNAPSHOT_BEGIN, SNAPSHOT_END};

const SYMBOL: &str = "AAPL{=5m}";
const WAIT: Duration = Duration::from_secs(5);

/// Connects and configures a Candle feed. Returns the stream **unread**: not
/// reading it while the burst lands is the pattern the issue is about.
async fn open_candle_feed(client: &mut DXLinkClient) -> (Receiver<MarketEvent>, u32) {
    let stream = client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Candle])
        .await
        .expect("failed to set up feed");
    (stream, channel_id)
}

/// Registers a counting callback on the symbol and subscribes to its history.
/// Returns the callback's tally: the delivery worker runs the callback before
/// it offers the event to the stream, so the tally says how far the burst got.
async fn subscribe_to_history(client: &mut DXLinkClient, channel_id: u32) -> Arc<AtomicUsize> {
    let seen = Arc::new(AtomicUsize::new(0));
    let tally = seen.clone();
    client.on_event(SYMBOL, move |_| {
        tally.fetch_add(1, Ordering::SeqCst);
    });

    client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Candle".to_string(),
                symbol: SYMBOL.to_string(),
                from_time: Some(1_690_000_000_000),
                source: None,
            }],
        )
        .await
        .expect("failed to subscribe to history");

    seen
}

/// Polls `condition` until it holds or the test's patience runs out.
async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Takes everything the stream holds, then everything that arrives until it
/// has been quiet for a moment.
async fn drain(stream: &mut Receiver<MarketEvent>) -> Vec<MarketEvent> {
    let mut events = Vec::new();
    // A closed stream or a quiet 300 ms both end the drain.
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(300), stream.recv()).await
    {
        events.push(event);
    }
    events
}

/// Waits for the supervisor to report a completed reconnect, failing rather
/// than hanging on anything else that ends the attempt.
async fn wait_for_reconnected(states: &mut broadcast::Receiver<ConnectionState>) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, states.recv()).await {
            Ok(Ok(ConnectionState::Reconnected)) => return,
            Ok(Ok(ConnectionState::GaveUp { reason })) => {
                panic!("the reconnect gave up: {reason}")
            }
            // Lost and Reconnecting are on the way there; lagging is documented.
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                panic!("the state stream closed before the reconnect completed")
            }
            Err(_) => panic!("timed out waiting for the reconnect"),
        }
    }
}

fn flags_of(event: &MarketEvent) -> i64 {
    match event {
        MarketEvent::Candle(candle) => {
            assert_eq!(candle.event_symbol, SYMBOL);
            candle.event_flags
        }
        other => panic!("expected a candle, got {other:?}"),
    }
}

fn time_of(event: &MarketEvent) -> i64 {
    match event {
        MarketEvent::Candle(candle) => candle.time,
        other => panic!("expected a candle, got {other:?}"),
    }
}

/// The regression test for the issue: a whole snapshot lands while nobody is
/// reading, and it must still be there, terminator included, when they do.
#[tokio::test]
async fn test_a_history_burst_survives_until_the_consumer_reads() {
    let server = MockServer::start(Behaviour::HistoryBurst).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let (mut stream, channel_id) = open_candle_feed(&mut client).await;
    let seen = subscribe_to_history(&mut client, channel_id).await;

    // Let the entire burst go through the worker before touching the stream.
    wait_until("the callback to see the whole burst", || {
        seen.load(Ordering::SeqCst) == HISTORY_BURST_BARS
    })
    .await;

    let events = drain(&mut stream).await;

    assert_eq!(
        events.len(),
        HISTORY_BURST_BARS,
        "the default buffer must hold a day of 1-minute bars"
    );
    assert_eq!(
        client.dropped_event_count(),
        0,
        "nothing may be lost when the burst fits the buffer"
    );

    let first = events.first().expect("at least one bar");
    let last = events.last().expect("at least one bar");
    assert_eq!(
        flags_of(first),
        SNAPSHOT_BEGIN,
        "the first bar opens the snapshot"
    );
    assert_eq!(
        flags_of(last),
        SNAPSHOT_END,
        "the last bar closes the snapshot"
    );
    assert!(
        time_of(first) > time_of(last),
        "order is preserved: the venue replays newest first"
    );
    for event in &events[1..events.len() - 1] {
        assert_eq!(flags_of(event), 0, "no flags between the two markers");
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// The failure the issue describes, now observable: an undersized buffer keeps
/// exactly its capacity, the terminator is among the missing, and the counter
/// says how much was lost rather than a debug line nobody reads.
#[tokio::test]
async fn test_an_undersized_buffer_reports_what_it_dropped() {
    const CAPACITY: usize = 16;
    let expected_dropped = (HISTORY_BURST_BARS - CAPACITY) as u64;

    let server = MockServer::start(Behaviour::HistoryBurst).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token")
        .with_event_buffer(CAPACITY)
        .expect("a non-zero capacity is valid");
    let (mut stream, channel_id) = open_candle_feed(&mut client).await;
    subscribe_to_history(&mut client, channel_id).await;

    // The counter is the thing under test, so it is also the thing waited on.
    wait_until("every surplus event to be counted as dropped", || {
        client.dropped_event_count() == expected_dropped
    })
    .await;

    let events = drain(&mut stream).await;

    assert_eq!(
        events.len(),
        CAPACITY,
        "the buffer keeps exactly its capacity"
    );
    assert_eq!(
        events.len() as u64 + client.dropped_event_count(),
        HISTORY_BURST_BARS as u64,
        "every event is either delivered or counted, never silently gone"
    );
    assert_eq!(
        flags_of(&events[0]),
        SNAPSHOT_BEGIN,
        "the head of the burst is what survives"
    );
    assert!(
        events.iter().all(|event| flags_of(event) != SNAPSHOT_END),
        "the terminator is in the tail, which is what an overflow loses"
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// The counter is one client-wide total, not one per session: a rebuilt reader
/// keeps adding to it. The server hangs up after the first subscription, the
/// replayed subscription draws a second burst into a stream still holding the
/// first sixteen bars, so every bar of that burst is lost and the total has to
/// say so.
#[tokio::test]
async fn test_the_drop_count_is_cumulative_across_a_reconnect() {
    const CAPACITY: usize = 16;
    let first_session = (HISTORY_BURST_BARS - CAPACITY) as u64;
    let both_sessions = first_session + HISTORY_BURST_BARS as u64;

    let server = MockServer::start(Behaviour::HistoryBurstDroppingFirstSession).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token")
        .with_event_buffer(CAPACITY)
        .expect("a non-zero capacity is valid");
    client.with_reconnect(ReconnectPolicy {
        initial_delay: Duration::from_millis(50),
        max_delay: Duration::from_millis(200),
        max_attempts: Some(5),
        jitter: false,
    });
    let (mut stream, channel_id) = open_candle_feed(&mut client).await;
    // Taken after connect, before the subscription that makes the server hang
    // up, so nothing about the reconnect is missed.
    let mut states = client
        .connection_states()
        .expect("a policy opens the state stream");
    subscribe_to_history(&mut client, channel_id).await;

    // Reconnected is reported once the replay has gone out, so the second
    // burst is either on its way or already counted.
    wait_for_reconnected(&mut states).await;
    wait_until("both bursts to be counted on one total", || {
        client.dropped_event_count() == both_sessions
    })
    .await;

    let events = drain(&mut stream).await;
    assert_eq!(
        events.len(),
        CAPACITY,
        "the stream still holds the head of the first burst; the reconnect did not reset it"
    );
    assert_eq!(
        events.len() as u64 + client.dropped_event_count(),
        2 * HISTORY_BURST_BARS as u64,
        "two bursts, one total: delivered plus dropped covers both sessions"
    );

    client.disconnect().await.expect("failed to disconnect");
}
