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
                    "CHANNEL_REQUEST" => responses.push(json!({
                        "channel": channel,
                        "type": "CHANNEL_OPENED",
                        "service": value["service"].as_str().unwrap_or("FEED"),
                        "parameters": {}
                    })),
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
                            "dataFormat": "COMPACT"
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
                self.received()
            ),
        }
    }
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
