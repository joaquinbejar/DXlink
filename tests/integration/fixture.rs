//! The single offline DXLink server used by every integration test.
//!
//! Two properties matter more than convenience here.
//!
//! It binds an **ephemeral port**, so the suite stays parallel-safe. The
//! fixtures this replaces bound 3030 and 3031, which is why six connection
//! tests were disabled as "port conflicts".
//!
//! It emits COMPACT rows in **the exact field order the client asked for** in
//! its own `FEED_SETUP` message, rather than in an order hardcoded here. That
//! makes the event tests a real check of the `setup_feed` ↔ `parse_compact_data`
//! contract: if the requested field list and the parser stride ever diverge, the
//! decoded values stop matching and the test fails. The fixtures this replaces
//! hardcoded `[symbol, eventType, ...]`, the reverse of what `setup_feed` asks
//! for, which is why no test could assert a decoded symbol.

#![allow(dead_code)]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

/// How long a `wait_for` may block before the test is declared failed.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// What the server does beyond answering the protocol normally.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Behaviour {
    /// Plain, successful session.
    Normal,
    /// Send a Ping before and a Pong after every response. Control frames are
    /// ordinary traffic and must never break a session.
    ControlFrames,
    /// Answer `AUTH` with `UNAUTHORIZED` instead of `AUTHORIZED`.
    RejectAuth,
    /// Complete the session, then hang up once the feed is subscribed.
    CloseAfterSubscribe,
    /// Accept the socket and never say anything.
    Silent,
    /// Answer SETUP with a message of the wrong type.
    WrongTypeOnSetup,
    /// Answer SETUP on a channel other than 0.
    WrongChannelOnSetup,
    /// Answer SETUP with text that is not JSON.
    MalformedSetup,
    /// Answer SETUP with a protocol ERROR frame.
    ErrorOnSetup,
    /// Answer SETUP with an ERROR whose message echoes a credential back.
    ErrorEchoingToken,
    /// Answer a CHANNEL_REQUEST with a channel-scoped ERROR instead of
    /// CHANNEL_OPENED.
    ErrorOnChannelRequest,
    /// Accept CHANNEL_REQUEST but never answer it.
    IgnoreChannelRequest,
    /// Ignore the first CHANNEL_REQUEST and answer every one after it, so a
    /// test can prove a timed-out request does not steal the next response.
    IgnoreFirstChannelRequest,
    /// Complete the handshake and then hang up, so the next send fails.
    CloseAfterHandshake,
    /// Echo a FEED_CONFIG whose Quote field list is reordered, the shape that
    /// silently shifts every decoded value.
    ReorderedFeedConfig,
    /// Negotiate a data format this client cannot decode.
    NonCompactFeedConfig,
    /// Honour the first FEED_SETUP and reorder the reply to every one after it.
    ReorderedFeedConfigOnSecondSetup,
    /// Negotiate a 3 second keepalive deadline, below the 15s the client used
    /// to assume. Lets a test prove the negotiated value is honoured without
    /// waiting a minute for it.
    ShortKeepalive,
}

pub struct MockServer {
    pub address: SocketAddr,
    received: Arc<Mutex<Vec<Value>>>,
    arrived: Arc<Notify>,
}

impl MockServer {
    pub async fn start(behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind mock server");
        let address = listener.local_addr().expect("failed to read local addr");

        let received = Arc::new(Mutex::new(Vec::new()));
        let arrived = Arc::new(Notify::new());

        let received_task = received.clone();
        let arrived_task = arrived.clone();

        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                return;
            };

            // Field order per (channel, event type), learned from FEED_SETUP.
            let mut event_fields: HashMap<(u64, String), Vec<String>> = HashMap::new();
            let mut channel_requests_seen = 0u32;
            let mut feed_setups_seen = 0u32;

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
                arrived_task.notify_waiters();

                let channel = value["channel"].as_u64().unwrap_or(0);
                let mut responses: Vec<Value> = Vec::new();
                let mut close_after = false;

                // Handshake fault injection, before the normal answers.
                match (behaviour, value["type"].as_str().unwrap_or("")) {
                    (Behaviour::Silent, _) => continue,
                    (Behaviour::CloseAfterHandshake, "AUTH") => {
                        let _ = ws
                            .send(Message::Text(
                                json!({"channel": 0, "type": "AUTH_STATE",
                                       "state": "AUTHORIZED"})
                                .to_string()
                                .into(),
                            ))
                            .await;
                        let _ = ws.send(Message::Close(None)).await;
                        return;
                    }
                    (Behaviour::WrongTypeOnSetup, "SETUP") => {
                        let _ = ws
                            .send(Message::Text(
                                json!({"channel": 0, "type": "FEED_CONFIG"})
                                    .to_string()
                                    .into(),
                            ))
                            .await;
                        continue;
                    }
                    (Behaviour::WrongChannelOnSetup, "SETUP") => {
                        let _ = ws
                            .send(Message::Text(
                                json!({"channel": 7, "type": "SETUP", "version": "1.0"})
                                    .to_string()
                                    .into(),
                            ))
                            .await;
                        continue;
                    }
                    (Behaviour::MalformedSetup, "SETUP") => {
                        let _ = ws.send(Message::Text("not json at all".into())).await;
                        continue;
                    }
                    (Behaviour::ErrorEchoingToken, "SETUP") => {
                        let echoed = value["token"].as_str().unwrap_or("");
                        let _ = ws
                            .send(Message::Text(
                                json!({"channel": 0, "type": "ERROR", "error": "UNAUTHORIZED",
                                       "message": format!("rejected token {echoed}")})
                                .to_string()
                                .into(),
                            ))
                            .await;
                        continue;
                    }
                    (Behaviour::ErrorOnSetup, "SETUP") => {
                        let _ = ws
                            .send(Message::Text(
                                json!({"channel": 0, "type": "ERROR", "error": "UNAUTHORIZED",
                                       "message": "Authentication failed"})
                                .to_string()
                                .into(),
                            ))
                            .await;
                        continue;
                    }
                    _ => {}
                }

                match value["type"].as_str().unwrap_or("") {
                    "SETUP" => {
                        let negotiated = if behaviour == Behaviour::ShortKeepalive {
                            3
                        } else {
                            60
                        };
                        responses.push(json!({
                            "channel": channel,
                            "type": "SETUP",
                            "version": "1.0.0",
                            "keepaliveTimeout": negotiated,
                            "acceptKeepaliveTimeout": negotiated
                        }));
                        responses.push(json!({
                            "channel": 0, "type": "AUTH_STATE", "state": "UNAUTHORIZED"
                        }));
                    }
                    "AUTH" => {
                        let state = if behaviour == Behaviour::RejectAuth {
                            "UNAUTHORIZED"
                        } else {
                            "AUTHORIZED"
                        };
                        responses.push(json!({
                            "channel": 0, "type": "AUTH_STATE",
                            "state": state, "userId": "test-user"
                        }));
                    }
                    "CHANNEL_REQUEST" if behaviour == Behaviour::IgnoreChannelRequest => {}
                    "CHANNEL_REQUEST" if behaviour == Behaviour::IgnoreFirstChannelRequest => {
                        channel_requests_seen += 1;
                        if channel_requests_seen == 1 {
                            continue;
                        }
                        responses.push(json!({
                            "channel": channel, "type": "CHANNEL_OPENED",
                            "service": "FEED", "parameters": {}
                        }));
                    }
                    "CHANNEL_REQUEST" if behaviour == Behaviour::ErrorOnChannelRequest => {
                        responses.push(json!({
                            "channel": channel, "type": "ERROR",
                            "error": "BAD_ACTION", "message": "contract not supported"
                        }));
                    }
                    "CHANNEL_REQUEST" => responses.push(json!({
                        "channel": channel,
                        "type": "CHANNEL_OPENED",
                        "service": value["service"].as_str().unwrap_or("FEED"),
                        "parameters": {}
                    })),
                    "FEED_SETUP"
                        if behaviour == Behaviour::ReorderedFeedConfig
                            || (behaviour == Behaviour::ReorderedFeedConfigOnSecondSetup && {
                                feed_setups_seen += 1;
                                feed_setups_seen > 1
                            }) =>
                    {
                        let mut reordered = value["acceptEventFields"]["Quote"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        // Guard: a fixture panic would mask the behaviour under
                        // test rather than reporting it.
                        if reordered.len() >= 2 {
                            reordered.swap(0, 1);
                        }
                        responses.push(json!({
                            "channel": channel, "type": "FEED_CONFIG",
                            "aggregationPeriod": 0.1, "dataFormat": "COMPACT",
                            "eventFields": { "Quote": reordered }
                        }));
                    }
                    "FEED_SETUP" if behaviour == Behaviour::NonCompactFeedConfig => {
                        responses.push(json!({
                            "channel": channel, "type": "FEED_CONFIG",
                            "aggregationPeriod": 0.1, "dataFormat": "FULL"
                        }));
                    }
                    "FEED_SETUP" => {
                        // Remember exactly which fields the client asked for, in
                        // order: that is the wire layout it will decode against.
                        if let Some(fields) = value["acceptEventFields"].as_object() {
                            for (event_type, list) in fields {
                                let order: Vec<String> = list
                                    .as_array()
                                    .map(|a| {
                                        a.iter()
                                            .filter_map(|f| f.as_str().map(String::from))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                event_fields.insert((channel, event_type.clone()), order);
                            }
                        }
                        responses.push(json!({
                            "channel": channel,
                            "type": "FEED_CONFIG",
                            "aggregationPeriod": 0.1,
                            "dataFormat": "COMPACT",
                            "eventFields": value["acceptEventFields"].clone()
                        }));
                    }
                    "FEED_SUBSCRIPTION" => {
                        if let Some(add) = value.get("add").and_then(|a| a.as_array()) {
                            for sub in add {
                                let event_type = sub["type"].as_str().unwrap_or("");
                                let symbol = sub["symbol"].as_str().unwrap_or("");
                                let Some(order) =
                                    event_fields.get(&(channel, event_type.to_string()))
                                else {
                                    continue;
                                };
                                let row: Vec<Value> = order
                                    .iter()
                                    .map(|field| field_value(field, event_type, symbol))
                                    .collect();
                                responses.push(json!({
                                    "channel": channel,
                                    "type": "FEED_DATA",
                                    "data": [event_type, row]
                                }));
                            }
                        }
                        close_after = behaviour == Behaviour::CloseAfterSubscribe;
                    }
                    "CHANNEL_CANCEL" => responses.push(json!({
                        "channel": channel, "type": "CHANNEL_CLOSED"
                    })),
                    // KEEPALIVE and anything else needs no reply.
                    _ => {}
                }

                for response in responses {
                    if behaviour == Behaviour::ControlFrames {
                        let _ = ws.send(Message::Ping(vec![7].into())).await;
                    }
                    if ws
                        .send(Message::Text(response.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if behaviour == Behaviour::ControlFrames {
                        let _ = ws.send(Message::Pong(Vec::new().into())).await;
                    }
                }

                if close_after {
                    let _ = ws.send(Message::Close(None)).await;
                    return;
                }
            }
        });

        MockServer {
            address,
            received,
            arrived,
        }
    }

    pub fn url(&self) -> String {
        format!("ws://{}", self.address)
    }

    /// Every message the client has sent so far, in order.
    pub fn received(&self) -> Vec<Value> {
        self.received
            .lock()
            .expect("received lock poisoned")
            .clone()
    }

    /// Blocks until the client has sent a message matching `predicate`.
    ///
    /// This is what replaces the fixed sleeps the old suites used to
    /// "synchronise": the test proceeds as soon as the protocol message it
    /// depends on has actually arrived, and fails with a dump of everything
    /// received if it never does.
    pub async fn wait_for<F>(&self, what: &str, predicate: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        let wait = async {
            loop {
                // Register before scanning so a message arriving in between is
                // not missed.
                let notified = self.arrived.notified();
                if let Some(found) = self.received().into_iter().find(&predicate) {
                    return found;
                }
                notified.await;
            }
        };

        match tokio::time::timeout(WAIT_TIMEOUT, wait).await {
            Ok(found) => found,
            Err(_) => panic!(
                "timed out waiting for {what}; server received: {:#?}",
                redacted(self.received())
            ),
        }
    }
}

/// Masks credential fields before a failure dump reaches the test output.
///
/// The server records the client's outbound `AUTH`, so dumping everything it
/// received would print the bearer token. Harmless with the fixed test token,
/// not harmless the day someone points this fixture at a real one.
fn redacted(mut messages: Vec<Value>) -> Vec<Value> {
    fn mask(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, field) in map.iter_mut() {
                    if key.eq_ignore_ascii_case("token") {
                        *field = Value::String("<redacted>".to_string());
                    } else {
                        mask(field);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(mask),
            _ => {}
        }
    }

    messages.iter_mut().for_each(mask);
    messages
}

/// The value the server reports for one COMPACT column.
///
/// Keyed by wire field name so the row can be assembled in whatever order the
/// client requested. Anything unrecognised becomes null, which surfaces as a
/// decode failure rather than a plausible-looking number.
fn field_value(field: &str, event_type: &str, symbol: &str) -> Value {
    match field {
        "eventType" => json!(event_type),
        "eventSymbol" => json!(symbol),
        // Quote
        "bidPrice" => json!(150.25),
        "askPrice" => json!(150.5),
        "bidSize" => json!(100.0),
        "askSize" => json!(150.0),
        // Trade
        "price" => json!(151.25),
        "size" => json!(75.0),
        "dayVolume" => json!(10_000_000.0),
        // Greeks
        "delta" => json!(0.65),
        "gamma" => json!(0.05),
        "theta" => json!(-0.15),
        "vega" => json!(0.1),
        "rho" => json!(0.03),
        "volatility" => json!(0.25),
        _ => Value::Null,
    }
}

/// The values `field_value` reports, for assertions on the decoded event.
pub mod expected {
    pub const BID_PRICE: f64 = 150.25;
    pub const ASK_PRICE: f64 = 150.5;
    pub const BID_SIZE: f64 = 100.0;
    pub const ASK_SIZE: f64 = 150.0;
    pub const PRICE: f64 = 151.25;
    pub const SIZE: f64 = 75.0;
    pub const DAY_VOLUME: f64 = 10_000_000.0;
    pub const DELTA: f64 = 0.65;
    pub const GAMMA: f64 = 0.05;
    pub const THETA: f64 = -0.15;
    pub const VEGA: f64 = 0.1;
    pub const RHO: f64 = 0.03;
    pub const VOLATILITY: f64 = 0.25;
}

/// Convenience predicates for `wait_for`.
pub fn is_type(kind: &str) -> impl Fn(&Value) -> bool + '_ {
    move |m: &Value| m["type"].as_str() == Some(kind)
}

pub fn is_type_on_channel(kind: &str, channel: u32) -> impl Fn(&Value) -> bool + '_ {
    move |m: &Value| {
        m["type"].as_str() == Some(kind) && m["channel"].as_u64() == Some(channel as u64)
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::redacted;
    use serde_json::json;

    #[test]
    fn test_failure_dumps_do_not_carry_the_token() {
        let dumped = format!(
            "{:#?}",
            redacted(vec![
                json!({"type": "AUTH", "channel": 0, "token": "real-secret"})
            ])
        );

        assert!(!dumped.contains("real-secret"), "token leaked: {dumped}");
        assert!(dumped.contains("<redacted>"));
        // The rest of the message still has to be readable, that is the point
        // of the dump.
        assert!(dumped.contains("AUTH"));
    }
}
