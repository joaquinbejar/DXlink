//! Opt-in reconnection: the policy is off by default, and when it is on a
//! terminal socket failure is followed by a fresh handshake and a replay of
//! every channel, feed configuration and subscription.
//!
//! These drive a mock server that hangs up mid-session and then accepts the
//! client back, which is the only way to prove the replay is real rather than
//! the client simply not noticing the drop.

use crate::fixture::{Behaviour, MockServer, is_type_on_channel};
use dxlink::{
    ConnectionState, DXLinkClient, EventType, FeedSubscription, MarketEvent, ReconnectPolicy,
};
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
    // Generous: a reconnect can be waiting out a protocol deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
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
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
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

    let mut stream = client.connect().await.expect("failed to connect");
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

    // disconnect has to be able to stop the session the *supervisor* built, not
    // just the one connect spawned. A detached reader would keep reading and
    // the stream would never close.
    client.disconnect().await.expect("failed to disconnect");
    // Drained rather than checked once: the replay produced events, and it is
    // the stream *ending* that proves the rebuilt session was torn down.
    tokio::time::timeout(Duration::from_secs(5), async {
        while stream.recv().await.is_some() {}
    })
    .await
    .expect("the rebuilt session outlived disconnect");
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

/// Sets up a session that is about to be dropped, returning the channel and the
/// state stream.
async fn dropped_session(
    server: &MockServer,
    policy: ReconnectPolicy,
) -> (
    DXLinkClient,
    u32,
    tokio::sync::mpsc::Receiver<MarketEvent>,
    tokio::sync::mpsc::Receiver<ConnectionState>,
) {
    let mut client = DXLinkClient::new(&server.url(), "test-token");
    client.with_reconnect(policy);
    let events = client.connect().await.expect("failed to connect");
    let states = client
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

    (client, channel_id, events, states)
}

/// A peer that accepts the reconnect and then says nothing is exactly what
/// reconnection is for. It used to give up after one attempt, because a read
/// timeout is not a *terminal* error and the supervisor was asking that
/// question instead of "is this worth retrying".
#[tokio::test]
async fn test_a_silent_peer_is_retried_up_to_the_limit() {
    let server = MockServer::start(Behaviour::SilentOnReconnect).await;
    let (_client, _channel_id, _events, mut states) = dropped_session(
        &server,
        ReconnectPolicy {
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(10),
            max_attempts: Some(2),
            jitter: false,
        },
    )
    .await;

    let mut attempts = Vec::new();
    let reason = loop {
        let state = tokio::time::timeout(Duration::from_secs(60), states.recv())
            .await
            .expect("timed out waiting for the client to stop trying")
            .expect("the state stream closed early");
        match state {
            ConnectionState::Reconnecting { attempt, .. } => attempts.push(attempt),
            ConnectionState::GaveUp { reason } => break reason,
            ConnectionState::Reconnected => panic!("a silent peer must not count as reconnected"),
            _ => {}
        }
    };

    assert_eq!(
        attempts,
        [1, 2],
        "the policy's limit has to be honoured, not cut short after one"
    );
    assert!(
        reason.contains("after 2 attempt(s)"),
        "it should have stopped on the limit, not on a classification: {reason}"
    );
}

/// A replay that fails partway must leave the next attempt able to rebuild.
/// Clearing a channel's stored layout before asking for a new one made a failure
/// there destructive: the retry saw no layout, skipped feed setup and the
/// subscriptions, and reported success having only reopened the channel.
#[tokio::test]
async fn test_a_failed_replay_can_still_be_retried() {
    let server = MockServer::start(Behaviour::IgnoreFeedSetupOnReconnect).await;
    let (client, channel_id, _events, mut states) = dropped_session(
        &server,
        ReconnectPolicy {
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(10),
            // The second connection never answers FEED_SETUP; the third does.
            max_attempts: Some(4),
            jitter: false,
        },
    )
    .await;

    // The first attempt waits out the ten second FEED_SETUP deadline before the
    // second one can succeed, so this is deliberately patient.
    wait_for_state(&mut states, "the reconnect to complete", |state| {
        matches!(state, ConnectionState::Reconnected)
    })
    .await;

    // The proof that the retry rebuilt rather than skipped: the feed was set up
    // again after the rejected attempt, and the subscription went with it.
    let sent = wait_until(&server, "the second replay", |messages| {
        messages
            .iter()
            .filter(|m| {
                is_type_on_channel("FEED_SUBSCRIPTION", channel_id)(m) && m.get("add").is_some()
            })
            .count()
            == 2
    })
    .await;

    let setups = sent
        .iter()
        .filter(|m| is_type_on_channel("FEED_SETUP", channel_id)(m))
        .count();
    assert_eq!(
        setups, 3,
        "the timed-out attempt and the successful one both have to reconfigure"
    );
    assert_eq!(client.subscriptions(channel_id).len(), 1);
}

/// `disconnect` has to stop the session the **supervisor** built, not just the
/// one `connect` spawned. Dropping the rebuilt handles detaches those tasks, and
/// a detached reader holds the socket open long after the client believes it has
/// shut down.
///
/// The event stream closing does not prove this on its own, because `disconnect`
/// aborts the delivery worker regardless. What does prove it: the mock serves
/// one connection at a time, so it only reaches the next `accept` once the old
/// socket is really gone. A fresh client connecting is that proof.
#[tokio::test]
async fn test_disconnect_releases_the_reconnected_session() {
    let server = MockServer::start(Behaviour::DropFirstSession).await;
    let (mut client, _channel_id, _events, mut states) =
        dropped_session(&server, prompt_policy()).await;

    wait_for_state(&mut states, "the reconnect to complete", |state| {
        matches!(state, ConnectionState::Reconnected)
    })
    .await;

    client.disconnect().await.expect("failed to disconnect");

    // A detached reader would still own its half of the socket, and the default
    // sixty second silence deadline means it would sit there for a minute.
    let mut next = DXLinkClient::new(&server.url(), "test-token");
    tokio::time::timeout(Duration::from_secs(5), next.connect())
        .await
        .expect("the reconnected session still held the socket after disconnect")
        .expect("the fresh client failed to connect");

    next.disconnect().await.expect("failed to disconnect");
}

/// Dropping a client mid-attempt is the case the shutdown channel alone does not
/// cover: the supervisor is inside a handshake rather than waiting on `select!`,
/// so nothing wakes it until that handshake finishes ten seconds later — long
/// enough to install a session for a client that no longer exists. Only the
/// abort in `Drop` cuts it short.
#[tokio::test]
async fn test_dropping_a_client_stops_a_supervisor_mid_attempt() {
    let server = MockServer::start(Behaviour::SilentOnReconnect).await;
    let (client, _channel_id, events, mut states) = dropped_session(
        &server,
        ReconnectPolicy {
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(10),
            max_attempts: None,
            jitter: false,
        },
    )
    .await;

    // Once the attempt is announced the supervisor is about to be stuck inside
    // the handshake against a peer that never answers.
    wait_for_state(&mut states, "the first attempt", |state| {
        matches!(state, ConnectionState::Reconnecting { .. })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    drop(client);

    // Well under the handshake deadline it would otherwise be sitting in.
    assert!(
        tokio::time::timeout(Duration::from_secs(3), states.recv())
            .await
            .expect("the supervisor was still inside its attempt after the client was dropped")
            .is_none(),
        "the supervisor outlived the client"
    );
    drop(events);
}

/// Dropping a client without disconnecting must not leave a supervisor holding
/// the token and the delivery sender, able to reconnect after its owner is gone.
#[tokio::test]
async fn test_dropping_a_client_stops_its_supervisor() {
    let server = MockServer::start(Behaviour::DropFirstSession).await;
    let (client, _channel_id, events, mut states) = dropped_session(
        &server,
        ReconnectPolicy {
            initial_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(30),
            max_attempts: None,
            jitter: false,
        },
    )
    .await;

    wait_for_state(&mut states, "the backoff to start", |state| {
        matches!(state, ConnectionState::Reconnecting { .. })
    })
    .await;

    drop(client);

    // Both streams end because the supervisor was aborted and released the
    // senders it held. Without the Drop teardown it would still be sleeping.
    assert!(
        tokio::time::timeout(Duration::from_secs(5), states.recv())
            .await
            .expect("the state stream should close when the client is dropped")
            .is_none(),
        "the supervisor outlived the client"
    );
    drop(events);
}

/// A policy asking for no delay at all must not turn an outage into a spin.
#[tokio::test]
async fn test_a_zero_delay_policy_does_not_spin() {
    let server = MockServer::start(Behaviour::RejectAuthOnReconnect).await;
    let policy = ReconnectPolicy {
        initial_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
        max_attempts: Some(1),
        jitter: false,
    };

    let (_client, _channel_id, _events, mut states) = dropped_session(&server, policy).await;

    let state = wait_for_state(&mut states, "the first attempt", |state| {
        matches!(state, ConnectionState::Reconnecting { .. })
    })
    .await;

    match state {
        ConnectionState::Reconnecting { delay, .. } => assert!(
            delay >= Duration::from_millis(50),
            "a zero delay has to be clamped, got {delay:?}"
        ),
        other => panic!("expected Reconnecting, got {other:?}"),
    }
}
