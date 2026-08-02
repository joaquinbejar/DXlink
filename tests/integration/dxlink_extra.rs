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
