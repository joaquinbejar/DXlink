//! Subscription-management integration tests: unsubscribe, reset, and
//! historical (`fromTime`) subscriptions.
//!
//! Every assertion here used to be either commented out or replaced by a
//! warning log, which is why subscription bugs could not fail the suite.

use crate::fixture::{Behaviour, MockServer, is_type_on_channel};
use dxlink::{DXLinkClient, EventType, FeedSubscription};

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

/// Historical data is requested by adding `fromTime`, but this client has no
/// Candle decoder yet, so the request is refused rather than accepted into a
/// stream that can never produce. Restore the delivery assertions here when the
/// Candle decoder lands.
#[tokio::test]
async fn test_historical_subscription_is_refused_without_a_decoder() {
    let server = MockServer::start(Behaviour::Normal).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.connect().await.expect("failed to connect");
    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");

    // Configuring the channel for Candle is refused first.
    assert!(
        client
            .setup_feed(channel_id, &[EventType::Candle])
            .await
            .is_err(),
        "a type with no decoder must not be configurable"
    );

    // And so is subscribing, on a channel configured for something else.
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");

    let result = client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Candle".to_string(),
                symbol: "AAPL{=5m}".to_string(),
                from_time: Some(1_700_000_000_000),
                source: None,
            }],
        )
        .await;
    assert!(
        result.is_err(),
        "an undecodable subscription must be refused"
    );

    client.disconnect().await.expect("failed to disconnect");
}
