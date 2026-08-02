//! Subscription-management integration tests: unsubscribe, reset, and
//! historical (`fromTime`) subscriptions.
//!
//! Every assertion here used to be either commented out or replaced by a
//! warning log, which is why subscription bugs could not fail the suite.

use crate::fixture::{Behaviour, MockServer, expected, is_type_on_channel};
use dxlink::events::CandleEvent;
use dxlink::{DXLinkClient, EventType, FeedSubscription, MarketEvent};
use std::sync::{Arc, Mutex};

/// Opens a connected client with a configured feed channel.
async fn connected_feed(server: &MockServer, events: &[EventType]) -> (DXLinkClient, u32) {
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, events)
        .await
        .expect("failed to set up feed");
    (client, channel_id)
}

fn quote_sub(symbol: &str) -> FeedSubscription {
    FeedSubscription {
        event_type: "Quote".to_string(),
        symbol: symbol.to_string(),
        from_time: None,
        source: None,
    }
}

#[tokio::test]
async fn test_unsubscribe_sends_a_remove_for_the_right_symbol() {
    let server = MockServer::start(Behaviour::Normal).await;
    let (mut client, channel_id) = connected_feed(&server, &[EventType::Quote]).await;

    client
        .subscribe(channel_id, vec![quote_sub("AAPL"), quote_sub("MSFT")])
        .await
        .expect("failed to subscribe");

    server
        .wait_for("the add subscription", |m| {
            is_type_on_channel("FEED_SUBSCRIPTION", channel_id)(m) && m.get("add").is_some()
        })
        .await;

    client
        .unsubscribe(channel_id, vec![quote_sub("AAPL")])
        .await
        .expect("failed to unsubscribe");

    let removal = server
        .wait_for("the remove subscription", |m| {
            is_type_on_channel("FEED_SUBSCRIPTION", channel_id)(m) && m.get("remove").is_some()
        })
        .await;

    let removed = removal["remove"]
        .as_array()
        .expect("remove should be a list");
    assert_eq!(removed.len(), 1, "only AAPL was unsubscribed: {removed:?}");
    assert_eq!(removed[0]["symbol"], "AAPL");
    assert_eq!(removed[0]["type"], "Quote");

    client.disconnect().await.expect("failed to disconnect");
}

#[tokio::test]
async fn test_reset_subscriptions_sends_the_reset_flag() {
    let server = MockServer::start(Behaviour::Normal).await;
    let (mut client, channel_id) = connected_feed(&server, &[EventType::Quote]).await;

    client
        .subscribe(channel_id, vec![quote_sub("AAPL")])
        .await
        .expect("failed to subscribe");
    server
        .wait_for("the add subscription", |m| {
            is_type_on_channel("FEED_SUBSCRIPTION", channel_id)(m) && m.get("add").is_some()
        })
        .await;

    client
        .reset_subscriptions(channel_id)
        .await
        .expect("failed to reset subscriptions");

    let reset = server
        .wait_for("the reset subscription", |m| {
            is_type_on_channel("FEED_SUBSCRIPTION", channel_id)(m) && m.get("reset").is_some()
        })
        .await;

    assert_eq!(
        reset["reset"], true,
        "reset must be sent as true, got {reset:#?}"
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// Checks every column of a decoded bar. Both delivery paths run it, so a
/// stride drift cannot pass by only being asserted on one of them.
fn assert_full_candle(candle: &CandleEvent, path: &str) {
    assert_eq!(candle.event_symbol, "AAPL{=5m}", "wrong symbol on {path}");
    assert_eq!(candle.event_time, expected::EVENT_TIME, "on {path}");
    assert_eq!(candle.event_flags, expected::EVENT_FLAGS, "on {path}");
    assert_eq!(candle.index, expected::INDEX, "on {path}");
    assert_eq!(candle.time, expected::TIME, "on {path}");
    assert_eq!(candle.sequence, expected::SEQUENCE, "on {path}");
    assert_eq!(candle.count, expected::COUNT, "on {path}");
    assert_eq!(candle.open, expected::OPEN, "on {path}");
    assert_eq!(candle.high, expected::HIGH, "on {path}");
    assert_eq!(candle.low, expected::LOW, "on {path}");
    assert_eq!(candle.close, expected::CLOSE, "on {path}");
    assert_eq!(candle.volume, expected::VOLUME, "on {path}");
    assert_eq!(candle.vwap, expected::VWAP, "on {path}");
    assert_eq!(candle.bid_volume, expected::BID_VOLUME, "on {path}");
    assert_eq!(candle.ask_volume, expected::ASK_VOLUME, "on {path}");
    assert_eq!(candle.imp_volatility, expected::IMP_VOLATILITY, "on {path}");
    assert_eq!(candle.open_interest, expected::OPEN_INTEREST, "on {path}");
}

/// Historical data is requested by adding `fromTime`, and now that Candle rows
/// decode, the bars must actually arrive. The refusal this replaces was the
/// honest behaviour while there was no decoder.
#[tokio::test]
async fn test_historical_candles_are_requested_and_delivered() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut stream = client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Candle])
        .await
        .expect("Candle has a decoder now, so configuring it must work");

    // Fixed value rather than "now": the assertion is about faithful transport.
    let from_time: i64 = 1_700_000_000_000;

    // The callback path routes by symbol through `symbol_of`, which needed a new
    // arm for this variant. Registering here covers that arm; without it a
    // missing arm would only show up as callbacks silently never firing.
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let sink = delivered.clone();
    client.on_event("AAPL{=5m}", move |event| {
        sink.lock().expect("callback lock poisoned").push(event);
    });

    client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Candle".to_string(),
                symbol: "AAPL{=5m}".to_string(),
                from_time: Some(from_time),
                source: None,
            }],
        )
        .await
        .expect("failed to subscribe to historical data");

    let subscription = server
        .wait_for("the historical subscription", |m| {
            is_type_on_channel("FEED_SUBSCRIPTION", channel_id)(m) && m.get("add").is_some()
        })
        .await;

    let added = subscription["add"]
        .as_array()
        .expect("add should be a list");
    assert_eq!(added[0]["symbol"], "AAPL{=5m}");
    assert_eq!(
        added[0]["fromTime"].as_i64(),
        Some(from_time),
        "fromTime must reach the wire unchanged"
    );

    // And the bar comes back decoded.
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv())
        .await
        .expect("no candle arrived")
        .expect("the stream closed");

    match &event {
        MarketEvent::Candle(candle) => assert_full_candle(candle, "the stream"),
        other => panic!("expected a candle, got {other:?}"),
    }

    // The worker awaits the callback before pushing to the stream, so by the
    // time the bar above arrived the callback had already run. Copy out of the
    // lock in its own scope so no guard reaches the await below.
    let seen: Vec<MarketEvent> = {
        let events = delivered.lock().expect("callback lock poisoned");
        events.clone()
    };
    match seen.as_slice() {
        [MarketEvent::Candle(candle)] => assert_full_candle(candle, "the callback"),
        other => {
            panic!("the callback for AAPL{{=5m}} should have received one candle, got {other:?}")
        }
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// Opens a second feed channel on an already connected client.
async fn second_feed(client: &mut DXLinkClient, events: &[EventType]) -> u32 {
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create the second feed channel");
    client
        .setup_feed(channel_id, events)
        .await
        .expect("failed to set up the second feed");
    channel_id
}

#[tokio::test]
async fn test_two_channels_track_the_same_symbol_independently() {
    let server = MockServer::start(Behaviour::Normal).await;
    let (mut client, first) = connected_feed(&server, &[EventType::Quote]).await;
    let second = second_feed(&mut client, &[EventType::Quote]).await;
    assert_ne!(first, second, "the channels have to be distinct");

    client
        .subscribe(first, vec![quote_sub("AAPL")])
        .await
        .expect("failed to subscribe on the first channel");
    client
        .subscribe(second, vec![quote_sub("AAPL")])
        .await
        .expect("failed to subscribe on the second channel");

    // One entry each, not one shared entry: the old global set collapsed these
    // two into one and could not tell them apart afterwards.
    assert_eq!(client.subscriptions(first).len(), 1);
    assert_eq!(client.subscriptions(second).len(), 1);
    assert_eq!(client.subscribed_channels(), vec![first, second]);

    client.disconnect().await.expect("failed to disconnect");
}

#[tokio::test]
async fn test_resetting_one_channel_leaves_the_other_alone() {
    let server = MockServer::start(Behaviour::Normal).await;
    let (mut client, first) = connected_feed(&server, &[EventType::Quote]).await;
    let second = second_feed(&mut client, &[EventType::Quote]).await;

    client
        .subscribe(first, vec![quote_sub("AAPL"), quote_sub("MSFT")])
        .await
        .expect("failed to subscribe on the first channel");
    client
        .subscribe(second, vec![quote_sub("TSLA")])
        .await
        .expect("failed to subscribe on the second channel");

    client
        .reset_subscriptions(first)
        .await
        .expect("failed to reset");

    assert!(
        client.subscriptions(first).is_empty(),
        "the reset channel should be empty"
    );
    let survivors = client.subscriptions(second);
    assert_eq!(survivors.len(), 1, "the other channel lost its state");
    assert_eq!(survivors[0].symbol, "TSLA");
    assert_eq!(client.subscribed_channels(), vec![second]);

    client.disconnect().await.expect("failed to disconnect");
}

#[tokio::test]
async fn test_closing_a_channel_forgets_only_its_subscriptions() {
    let server = MockServer::start(Behaviour::Normal).await;
    let (mut client, first) = connected_feed(&server, &[EventType::Quote]).await;
    let second = second_feed(&mut client, &[EventType::Quote]).await;

    client
        .subscribe(first, vec![quote_sub("AAPL")])
        .await
        .expect("failed to subscribe on the first channel");
    client
        .subscribe(second, vec![quote_sub("TSLA")])
        .await
        .expect("failed to subscribe on the second channel");

    client
        .close_channel(first)
        .await
        .expect("failed to close the channel");

    assert!(
        client.subscriptions(first).is_empty(),
        "a closed channel delivers nothing, so it holds nothing"
    );
    assert_eq!(client.subscriptions(second).len(), 1);

    client.disconnect().await.expect("failed to disconnect");
}

#[tokio::test]
async fn test_a_historical_subscription_keeps_its_from_time_and_source() {
    let server = MockServer::start(Behaviour::Normal).await;
    let (mut client, channel_id) = connected_feed(&server, &[EventType::Candle]).await;

    let from_time = 1_700_000_000_000;
    client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Candle".to_string(),
                symbol: "AAPL{=5m}".to_string(),
                from_time: Some(from_time),
                source: Some("DEX".to_string()),
            }],
        )
        .await
        .expect("failed to subscribe");

    // Both have to survive: a replay that dropped them would resubscribe live
    // where the consumer had asked for history.
    let tracked = client.subscriptions(channel_id);
    assert_eq!(tracked.len(), 1);
    assert_eq!(tracked[0].symbol, "AAPL{=5m}");
    assert_eq!(tracked[0].from_time, Some(from_time));
    assert_eq!(tracked[0].source.as_deref(), Some("DEX"));

    client.disconnect().await.expect("failed to disconnect");
}

#[tokio::test]
async fn test_resubscribing_replaces_the_earlier_entry() {
    let server = MockServer::start(Behaviour::Normal).await;
    let (mut client, channel_id) = connected_feed(&server, &[EventType::Candle]).await;

    let candle = |from_time: i64| FeedSubscription {
        event_type: "Candle".to_string(),
        symbol: "AAPL{=5m}".to_string(),
        from_time: Some(from_time),
        source: None,
    };

    client
        .subscribe(channel_id, vec![candle(1_700_000_000_000)])
        .await
        .expect("failed to subscribe");
    client
        .subscribe(channel_id, vec![candle(1_700_000_600_000)])
        .await
        .expect("failed to resubscribe");

    // The server keys a subscription by type and symbol within a channel, so
    // the second call supersedes the first. Tracking both would make a replay
    // send a subscription the server had already replaced.
    let tracked = client.subscriptions(channel_id);
    assert_eq!(tracked.len(), 1, "the entry should have been replaced");
    assert_eq!(tracked[0].from_time, Some(1_700_000_600_000));

    client.disconnect().await.expect("failed to disconnect");
}

#[tokio::test]
async fn test_subscriptions_come_back_in_the_order_they_were_asked_for() {
    let server = MockServer::start(Behaviour::Normal).await;
    let (mut client, channel_id) = connected_feed(&server, &[EventType::Quote]).await;

    for symbol in ["MSFT", "AAPL", "TSLA", "NVDA"] {
        client
            .subscribe(channel_id, vec![quote_sub(symbol)])
            .await
            .expect("failed to subscribe");
    }

    // Not sorted, not hash order: the order the consumer built the session in,
    // which is the order a replay has to send them back in.
    let symbols: Vec<String> = client
        .subscriptions(channel_id)
        .into_iter()
        .map(|sub| sub.symbol)
        .collect();
    assert_eq!(symbols, ["MSFT", "AAPL", "TSLA", "NVDA"]);

    client.disconnect().await.expect("failed to disconnect");
}

/// A send that never left the client must not be recorded as one that did.
#[tokio::test]
async fn test_a_failed_send_does_not_move_the_tracked_state() {
    let server = MockServer::start(Behaviour::CloseAfterSubscribe).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut stream = client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");

    client
        .subscribe(channel_id, vec![quote_sub("AAPL")])
        .await
        .expect("the first subscribe happens before the server hangs up");
    assert_eq!(client.subscriptions(channel_id).len(), 1);

    // The stream closing is the documented signal that the session is over, so
    // waiting on it makes the next send deterministically fail.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while stream.recv().await.is_some() {}
    })
    .await
    .expect("the stream never closed after the server hung up");

    assert!(
        client
            .unsubscribe(channel_id, vec![quote_sub("AAPL")])
            .await
            .is_err(),
        "unsubscribing over a dead socket has to fail"
    );
    assert_eq!(
        client.subscriptions(channel_id).len(),
        1,
        "a failed unsubscribe must not forget a live subscription"
    );

    assert!(
        client
            .subscribe(channel_id, vec![quote_sub("MSFT")])
            .await
            .is_err(),
        "subscribing over a dead socket has to fail"
    );
    let symbols: Vec<String> = client
        .subscriptions(channel_id)
        .into_iter()
        .map(|sub| sub.symbol)
        .collect();
    assert_eq!(
        symbols,
        ["AAPL"],
        "a failed subscribe must not be recorded as one that happened"
    );

    assert!(
        client.reset_subscriptions(channel_id).await.is_err(),
        "resetting over a dead socket has to fail"
    );
    assert_eq!(
        client.subscriptions(channel_id).len(),
        1,
        "a failed reset must not clear the tracked state"
    );
}
