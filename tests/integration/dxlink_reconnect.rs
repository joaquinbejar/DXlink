//! Opt-in reconnection: the policy is off by default, and when it is on a
//! terminal socket failure is followed by a fresh handshake and a replay of
//! every channel, feed configuration and subscription.
//!
//! These drive a mock server that hangs up mid-session and then accepts the
//! client back, which is the only way to prove the replay is real rather than
//! the client simply not noticing the drop.

use crate::fixture::{Behaviour, MockServer, is_type_on_channel};
use dxlink::{ConnectionState, DXLinkClient, EventType, FeedSubscription, ReconnectPolicy};
use std::time::Duration;

/// Fast and jitter-free, so a test asserts on behaviour rather than on timing.
fn prompt_policy() -> ReconnectPolicy {
    ReconnectPolicy {
        initial_delay: Duration::from_millis(10),
        max_delay: Duration::from_millis(50),
        max_attempts: Some(5),
        jitter: false,
    }
}

fn quote_sub(symbol: &str) -> FeedSubscription {
    FeedSubscription {
        event_type: "Quote".to_string(),
        symbol: symbol.to_string(),
        from_time: None,
        source: None,
    }
}

/// Waits for a state, failing rather than hanging if it never comes.
async fn wait_for_state(
    states: &mut tokio::sync::mpsc::Receiver<ConnectionState>,
    what: &str,
    matches: impl Fn(&ConnectionState) -> bool,
) -> ConnectionState {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let state = tokio::time::timeout(remaining, states.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap_or_else(|| panic!("the state stream closed before {what}"));
        if matches(&state) {
            return state;
        }
    }
}

/// Waits until everything the server has received satisfies `matches`.
///
/// `MockServer::wait_for` looks at one message at a time; a replay is only
/// visible in the whole history, because the assertion is that a second one
/// arrived.
async fn wait_until(
    server: &MockServer,
    what: &str,
    matches: impl Fn(&[serde_json::Value]) -> bool,
) -> Vec<serde_json::Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let received = server.received();
        if matches(&received) {
            return received;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The default has to stay exactly what it was: no policy, no supervisor, and a
/// dead session that stays dead.
#[tokio::test]
async fn test_without_a_policy_a_lost_session_stays_lost() {
    let server = MockServer::start(Behaviour::DropFirstSession).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    let mut stream = client.connect().await.expect("failed to connect");

    assert!(
        client.connection_states().is_none(),
        "there is no state stream without a policy"
    );

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
        .expect("failed to subscribe");

    // The stream closing is the documented end-of-session signal, and it still
    // has to happen: nothing may reconnect behind the consumer's back.
    tokio::time::timeout(Duration::from_secs(10), async {
        while stream.recv().await.is_some() {}
    })
    .await
    .expect("the stream never closed, so something reconnected uninvited");

    assert!(
        client.disconnect_reason().is_some(),
        "a dead session has to say why"
    );
}

#[tokio::test]
async fn test_reconnect_replays_channels_feeds_and_subscriptions() {
    let server = MockServer::start(Behaviour::DropFirstSession).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.with_reconnect(prompt_policy());

    let _stream = client.connect().await.expect("failed to connect");
    let mut states = client
        .connection_states()
        .expect("a policy means a state stream");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create feed channel");
    client
        .setup_feed(channel_id, &[EventType::Quote])
        .await
        .expect("failed to set up feed");
    client
        .subscribe(channel_id, vec![quote_sub("AAPL"), quote_sub("MSFT")])
        .await
        .expect("failed to subscribe");

    wait_for_state(&mut states, "the session to be reported lost", |state| {
        matches!(state, ConnectionState::Lost { .. })
    })
    .await;
    wait_for_state(&mut states, "the reconnect to complete", |state| {
        matches!(state, ConnectionState::Reconnected)
    })
    .await;

    // Reconnected is reported the moment the replay is sent, so give the
    // server the moment it needs to have read it before counting.
    let sent = wait_until(&server, "the replayed subscription", |messages| {
        messages
            .iter()
            .filter(|m| {
                is_type_on_channel("FEED_SUBSCRIPTION", channel_id)(m) && m.get("add").is_some()
            })
            .count()
            == 2
    })
    .await;
    let handshakes = sent
        .iter()
        .filter(|m| m["type"] == "SETUP" && m.get("version").is_some())
        .count();
    assert_eq!(handshakes, 2, "the reconnect has to redo the handshake");

    let reopened = sent
        .iter()
        .filter(|m| is_type_on_channel("CHANNEL_REQUEST", channel_id)(m))
        .count();
    assert_eq!(reopened, 2, "the channel has to be reopened");

    let reconfigured = sent
        .iter()
        .filter(|m| is_type_on_channel("FEED_SETUP", channel_id)(m))
        .count();
    assert_eq!(reconfigured, 2, "the feed has to be reconfigured");

    let replays: Vec<&serde_json::Value> = sent
        .iter()
        .filter(|m| {
            is_type_on_channel("FEED_SUBSCRIPTION", channel_id)(m) && m.get("add").is_some()
        })
        .collect();
    assert_eq!(replays.len(), 2, "the subscriptions have to be replayed");

    let replayed = replays[1]["add"].as_array().expect("add should be a list");
    let symbols: Vec<&str> = replayed
        .iter()
        .map(|sub| sub["symbol"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        symbols,
        ["AAPL", "MSFT"],
        "the replay has to keep the order the consumer subscribed in"
    );

    // And the client's own view survived the rebuild.
    assert_eq!(client.subscriptions(channel_id).len(), 2);
    assert!(
        client.disconnect_reason().is_none(),
        "a live session must not still carry the old failure"
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// A rejected token fails the same way every time, so retrying is noise. This
/// is the classification the policy documents.
#[tokio::test]
async fn test_an_authentication_rejection_is_not_retried() {
    let server = MockServer::start(Behaviour::RejectAuthOnReconnect).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.with_reconnect(prompt_policy());

    let _stream = client.connect().await.expect("failed to connect");
    let mut states = client
        .connection_states()
        .expect("a policy means a state stream");

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
        .expect("failed to subscribe");

    let state = wait_for_state(&mut states, "the client to give up", |state| {
        matches!(state, ConnectionState::GaveUp { .. })
    })
    .await;

    match state {
        ConnectionState::GaveUp { reason } => assert!(
            reason.contains("not retrying"),
            "the reason has to say it was a classification, not exhaustion: {reason}"
        ),
        other => panic!("expected GaveUp, got {other:?}"),
    }

    // One attempt, not five: the policy's limit is irrelevant here.
    let handshakes = server
        .received()
        .iter()
        .filter(|m| m["type"] == "AUTH")
        .count();
    assert_eq!(handshakes, 2, "it must not have tried the token again");
}

/// Backoff must not outlive an explicit disconnect: waiting out a full delay
/// before noticing would make shutdown feel hung.
#[tokio::test]
async fn test_disconnect_cancels_a_running_backoff() {
    let server = MockServer::start(Behaviour::DropFirstSession).await;
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.with_reconnect(ReconnectPolicy {
        // Long enough that a supervisor which did not watch for shutdown would
        // still be sleeping when the assertion below runs.
        initial_delay: Duration::from_secs(30),
        max_delay: Duration::from_secs(30),
        max_attempts: None,
        jitter: false,
    });

    let _stream = client.connect().await.expect("failed to connect");
    let mut states = client
        .connection_states()
        .expect("a policy means a state stream");

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
        .expect("failed to subscribe");

    wait_for_state(&mut states, "the backoff to start", |state| {
        matches!(state, ConnectionState::Reconnecting { .. })
    })
    .await;

    // Well inside the shutdown grace period the client falls back to aborting
    // the task with: two seconds proves the supervisor cooperated rather than
    // being killed after waiting out the full grace.
    tokio::time::timeout(Duration::from_secs(2), client.disconnect())
        .await
        .expect("disconnect waited out the backoff instead of cancelling it")
        .expect("failed to disconnect");

    // The supervisor is gone, so its end of the state stream is closed.
    assert!(
        tokio::time::timeout(Duration::from_secs(5), states.recv())
            .await
            .expect("the state stream should close promptly")
            .is_none(),
        "the supervisor is still running after disconnect"
    );
}
