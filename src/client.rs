/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use crate::connection::{WebSocketConnection, sanitize_server_text};
use crate::error::{DXLinkError, DXLinkResult};
use crate::events::{ALL_EVENT_TYPES, CompactData, EventType, MarketEvent};
use crate::messages::{
    AuthMessage, AuthStateMessage, BaseMessage, ChannelRequestMessage, ErrorMessage,
    FeedConfigMessage, FeedDataMessage, FeedSetupMessage, FeedSubscription,
    FeedSubscriptionMessage, KeepaliveMessage, ServerSetupMessage, SetupMessage,
};
use crate::utils::try_parse_negotiated;

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Default timeout for keep-alive messages in seconds.  If no keep-alive
/// message is received within this timeframe, the connection is considered closed.
const DEFAULT_KEEPALIVE_TIMEOUT: u32 = 60;

/// Fraction of the negotiated deadline at which maintenance is sent.
///
/// The specification only requires *some* outbound message before the server's
/// `keepaliveTimeout`. Sending three times per deadline leaves room for a lost
/// packet without being chatty. The previous fixed 15s ignored the negotiation
/// entirely, so a server asking for less than that could close the connection
/// while the client believed it was healthy.
const KEEPALIVE_DIVISOR: u32 = 3;

/// Never schedule maintenance faster than this, whatever the server negotiates.
const MIN_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);

/// The `version` advertised in `SETUP`.
///
/// The specification's format is `<protocol-version>-<implementation>/<client-version>`
/// (for example `0.1-js/1.0.0`): protocol `0.1`, this implementation identified
/// as `dxlink-rs`, and the crate version.
///
/// Built from `CARGO_PKG_VERSION` at compile time so bumping the crate updates
/// what the server sees without a second edit. The previous hardcoded
/// `1.0.2-dxlink-0.1.3` named a crate version three releases stale and did not
/// follow the format, which leaves a client unidentifiable in server-side
/// diagnostics. `concat!` needs literals, so the protocol and implementation
/// tokens are spelled out here rather than composed from constants.
const DEFAULT_CLIENT_VERSION: &str = concat!("0.1-dxlink-rs/", env!("CARGO_PKG_VERSION"));

/// How long any single handshake response may take before `connect()` gives up.
///
/// Every SETUP/AUTH read is bounded by this: a server that accepts the socket
/// and then says nothing must not hold a caller open forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long shutdown waits for a cooperative step before forcing it.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Capacity of the queue between the socket reader and the delivery worker.
///
/// Generous: it only has to absorb a burst while the worker is inside a user
/// callback. When it does fill, events are dropped rather than blocking the
/// reader, because a blocked reader stops answering protocol traffic and makes
/// unrelated channel operations time out.
const DELIVERY_QUEUE_CAPACITY: usize = 1024;

/// The shortest a reconnect will ever wait between attempts.
///
/// A policy may legitimately ask for a very short delay, but zero with
/// unlimited attempts is a tight loop that turns one outage into a request and
/// log flood. Clamped rather than rejected, so a policy stays a hint about
/// pacing rather than something that can fail to install.
const MIN_RECONNECT_DELAY: Duration = Duration::from_millis(50);

/// How many connection-state changes to buffer for a consumer.
///
/// Small on purpose: a consumer that is not reading these does not want a
/// backlog of them, and the supervisor must never block on one.
const CONNECTION_STATE_CAPACITY: usize = 16;

/// The main communication channel identifier. This is likely used for
/// primary message exchange between client and server.
const MAIN_CHANNEL: u32 = 0;

/// Type alias for a callback function that handles market events.
///
/// This type alias represents a boxed dynamic function that takes a `MarketEvent`
/// as an argument.  The function is required to be `Send`, `Sync`, and have a
/// static lifetime (`'static`).
///
/// `Send` and `Sync` ensure that the callback can be safely used in concurrent contexts.
/// The `'static` lifetime requirement means the callback doesn't borrow any data
/// that could outlive its use.
///
pub type EventCallback = Box<dyn Fn(MarketEvent) + Send + Sync + 'static>;

/// Represents the different types of responses that can be received.
/// Each variant of the enum carries specific data related to the response type:
#[derive(Debug)]
enum ResponseType {
    /// Indicates a channel has been opened. The `u32` value represents the channel identifier.
    ChannelOpened(u32),
    /// A feed configuration. Carries the whole message, not just the
    /// channel: the negotiated data format and field order are the point.
    FeedConfig(Box<FeedConfigMessage>),
    /// Indicates a channel has been closed. The `u32` value represents the channel identifier.
    ChannelClosed(u32),
    /// A server error. Kept as `(code, message)` rather than pre-formatted text
    /// so the caller can pick the right `DXLinkError` from the code instead of
    /// having to parse it back out of a sentence.
    Error(String, String),
    /// A generic response type for other cases. The `String` value contains the response data.  This variant is currently unused (`#[allow(dead_code)]`).
    #[allow(dead_code)]
    Other(String),
}

/// Represents a request for a specific response from a WebSocket stream.  This struct is used to await a particular
/// response type, optionally filtered by channel ID.  It includes a `oneshot::Sender` to send the
/// response back to the requester.
#[derive(Debug)]
struct ResponseRequest {
    /// Identity, so cleanup removes this exact registration and never a later
    /// one that happens to want the same message type on the same channel.
    id: u64,
    /// The expected type of the response message (e.g., "CHANNEL_OPENED", "FEED_CONFIG", etc.).  This string should match the expected
    /// response message type.
    expected_type: String,
    /// The expected channel ID for the response.  If `None`, the channel ID is not considered when matching responses.
    channel_id: Option<u32>,
    /// A `oneshot::Sender` used to send the `ResponseType` back to the requester once the expected response is received.
    response_sender: oneshot::Sender<ResponseType>,
}

/// Reads one handshake response and checks it is the message the protocol calls
/// for at this point.
///
/// The handshake used to deserialize whatever arrived straight into the type it
/// hoped for, so a server `ERROR` surfaced as `missing field \`state\`` and a
/// message on the wrong channel was accepted silently. Errors name the state,
/// what was expected and what arrived; none of them can carry the token.
async fn expect_handshake_message(
    connection: &WebSocketConnection,
    state: &str,
    expected_type: &str,
    expected_channel: u32,
) -> DXLinkResult<String> {
    let raw = match connection.receive_with_timeout(HANDSHAKE_TIMEOUT).await? {
        Some(raw) => raw,
        None => {
            return Err(DXLinkError::Timeout(format!(
                "timed out after {}s waiting for {} on channel {} during {}",
                HANDSHAKE_TIMEOUT.as_secs(),
                expected_type,
                expected_channel,
                state
            )));
        }
    };

    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        DXLinkError::Protocol(format!(
            "during {state}, expected {expected_type} on channel {expected_channel} \
             but the server sent malformed JSON: {e}"
        ))
    })?;

    let received_type = value["type"].as_str().unwrap_or("<missing type>");
    let received_channel = value["channel"].as_u64();

    // A server ERROR here is the server telling us why, not an unexpected frame.
    if received_type == "ERROR" {
        // Both fields are free text chosen by the server and end up in an error
        // the caller will very likely log, so they get the same treatment as a
        // close reason: a server echoing the token back must not turn our own
        // error reporting into a credential leak.
        let code = sanitize_server_text(value["error"].as_str().unwrap_or("unknown"));
        let message = sanitize_server_text(value["message"].as_str().unwrap_or(""));
        let detail = format!("during {state}, the server returned {code}: {message}");
        return Err(if code.eq_ignore_ascii_case("UNAUTHORIZED") {
            DXLinkError::Authentication(detail)
        } else {
            DXLinkError::Protocol(detail)
        });
    }

    if received_type != expected_type {
        return Err(DXLinkError::Protocol(format!(
            "during {state}, expected {expected_type} but received {received_type}"
        )));
    }

    if received_channel != Some(u64::from(expected_channel)) {
        return Err(DXLinkError::Protocol(format!(
            "during {state}, expected {expected_type} on channel {expected_channel} \
             but it arrived on channel {}",
            received_channel
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<missing>".to_string())
        )));
    }

    Ok(raw)
}

/// Clones the live connection out of a shared slot.
fn live_connection(slot: &SharedConnection) -> DXLinkResult<WebSocketConnection> {
    slot.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .ok_or_else(|| DXLinkError::Connection("Not connected to DXLink server".to_string()))
}

/// Spawns the maintenance task and returns its handle plus the channel that
/// stops it.
///
/// Free-standing so both the initial connect and a reconnect build the same
/// task, rather than a reconnect growing its own slightly different copy.
fn spawn_keepalive(
    connection: WebSocketConnection,
    keepalive_interval: Duration,
) -> (JoinHandle<()>, mpsc::Sender<()>) {
    // Crear un canal para señales de cierre
    let (tx, mut rx) = mpsc::channel::<()>(1);

    let keepalive_handle = tokio::spawn(async move {
        // Start one interval out: tokio's first tick is immediate, which
        // fired a redundant KEEPALIVE the instant the session opened.
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + keepalive_interval,
            keepalive_interval,
        );

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Any outbound message resets the server's idle timer,
                    // so traffic that already went out covers this beat.
                    if connection.since_last_send() < keepalive_interval {
                        debug!("Skipping keepalive, the socket was used recently");
                        continue;
                    }

                    let keepalive_msg = KeepaliveMessage {
                        channel: MAIN_CHANNEL,
                        message_type: "KEEPALIVE".to_string(),
                    };

                    match connection.send(&keepalive_msg).await {
                        Ok(_) => {
                            debug!("Sent keepalive message");
                        },
                        Err(e) => {
                            error!("Failed to send keepalive: {}", e);
                            break;
                        }
                    }
                }
                _ = rx.recv() => {
                    // Recibimos una señal para terminar
                    debug!("Keepalive task received shutdown signal");
                    break;
                }
            }
        }

        debug!("Keepalive task terminated");
    });

    (keepalive_handle, tx)
}

/// What the protocol reader needs to run one session.
///
/// A struct rather than eight positional arguments: the reader is spawned from
/// two places now, and a swapped pair of `Arc`s there would be a silent routing
/// bug rather than a type error.
struct ReaderSetup {
    connection: WebSocketConnection,
    shutdown_keepalive: Option<mpsc::Sender<()>>,
    delivery_tx: Sender<MarketEvent>,
    disconnect_reason: Arc<Mutex<Option<String>>>,
    response_requests: Arc<Mutex<Vec<ResponseRequest>>>,
    channel_schemas: ChannelSchemas,
    receive_deadline: Duration,
    /// Told once, when the session dies of a terminal failure.
    ///
    /// `None` for a client with no reconnect policy, which is what keeps the
    /// default behaviour exactly as it was: the task exits and nothing tries to
    /// rebuild anything.
    session_lost: Option<mpsc::Sender<String>>,
}

/// Spawns the task that reads the socket and routes protocol traffic.
///
/// Free-standing so the initial connect and a reconnect run the same reader
/// rather than a reconnect growing its own slightly different copy.
fn spawn_reader(setup: ReaderSetup) -> JoinHandle<()> {
    let ReaderSetup {
        connection,
        shutdown_keepalive,
        delivery_tx,
        disconnect_reason,
        response_requests,
        channel_schemas,
        receive_deadline,
        session_lost,
    } = setup;

    tokio::spawn(async move {
        // Set only where the session is actually over, which is not every error
        // the loop can see: a malformed frame is not a dead socket.
        #[allow(unused_assignments)]
        let mut terminal_reason: Option<String> = None;
        let mut dropped_events: u64 = 0;

        loop {
            let received = match connection.receive_with_timeout(receive_deadline).await {
                Ok(Some(msg)) => Ok(msg),
                Ok(None) => {
                    // Silence past the advertised deadline means the peer is
                    // gone even though the socket is still open. Terminal
                    // here specifically: a bare read timeout is not terminal
                    // in general, which is why this does not go through
                    // is_terminal().
                    let reason = DXLinkError::Timeout(format!(
                        "no message received on channel {} for {}s, the connection is assumed dead",
                        MAIN_CHANNEL,
                        receive_deadline.as_secs()
                    ));
                    error!("{}", reason);
                    record_reason(&disconnect_reason, &reason);
                    terminal_reason = Some(reason.to_string());
                    break;
                }
                Err(e) => Err(e),
            };

            match received {
                Ok(msg) => {
                    debug!("Received message: {}", msg);

                    // Procesar el mensaje
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg) {
                        // Identificar el tipo de mensaje
                        let msg_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        let channel = value
                            .get("channel")
                            .and_then(|v| v.as_u64())
                            .map(|c| c as u32);

                        // Route to a waiter first. A channel-scoped ERROR
                        // answers whatever operation is pending on that
                        // channel, whichever success message it asked for:
                        // otherwise the caller sat until its timeout while
                        // the reason was only logged.
                        {
                            let mut delivered = false;
                            {
                                let mut requests = recover(&response_requests);
                                let is_error = msg_type == "ERROR";

                                while let Some(idx) = requests.iter().position(|req| {
                                    let channel_matches =
                                        req.channel_id.is_none() || req.channel_id == channel;
                                    channel_matches
                                        && (req.expected_type == msg_type
                                            || (is_error && channel.is_some()))
                                }) {
                                    let request = requests.remove(idx);

                                    let response = match msg_type {
                                        "CHANNEL_OPENED" => {
                                            ResponseType::ChannelOpened(channel.unwrap_or(0))
                                        }
                                        "FEED_CONFIG" => {
                                            match serde_json::from_str::<FeedConfigMessage>(&msg) {
                                                Ok(config) => {
                                                    ResponseType::FeedConfig(Box::new(config))
                                                }
                                                Err(e) => ResponseType::Error(
                                                    "MALFORMED_FEED_CONFIG".to_string(),
                                                    e.to_string(),
                                                ),
                                            }
                                        }
                                        "CHANNEL_CLOSED" => {
                                            ResponseType::ChannelClosed(channel.unwrap_or(0))
                                        }
                                        "ERROR" => {
                                            let error = value
                                                .get("error")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown");
                                            let message = value
                                                .get("message")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            ResponseType::Error(
                                                sanitize_server_text(error),
                                                sanitize_server_text(message),
                                            )
                                        }
                                        _ => ResponseType::Other(msg.clone()),
                                    };

                                    // A gone receiver means that caller
                                    // already walked away. Keep looking
                                    // rather than swallowing a response a
                                    // live waiter is still owed.
                                    if request.response_sender.send(response).is_ok() {
                                        delivered = true;
                                        break;
                                    }
                                    debug!("Dropping a stale response registration");
                                }
                            }

                            if delivered {
                                continue;
                            }
                        }

                        // Si nadie esperaba este mensaje específicamente, procesarlo normalmente
                        match msg_type {
                            "FEED_CONFIG" => {
                                // Nobody was waiting, so this is the server
                                // reporting the layout it settled on. The demo
                                // feed does this routinely: it answers
                                // FEED_SETUP with nothing, then sends the real
                                // list once a subscription exists.
                                //
                                // Adopted, not refused. The decoder reads by
                                // field name, so serving fewer fields than we
                                // asked for is decodable — the missing ones
                                // read as "not provided". Invalidating instead
                                // meant the channel delivered nothing at all.
                                if let Some(ch) = channel {
                                    match serde_json::from_str::<FeedConfigMessage>(&msg) {
                                        Ok(config) => adopt_config(&channel_schemas, ch, &config),
                                        Err(e) => {
                                            // Unreadable is not agreement: stop
                                            // decoding rather than keep using a
                                            // layout the server may have moved
                                            // away from.
                                            if recover(&channel_schemas).remove(&ch).is_some() {
                                                error!(
                                                    "Channel {ch} sent an unreadable \
                                                     FEED_CONFIG ({e}); its data will be \
                                                     dropped rather than decoded against a \
                                                     layout that may be stale"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            "FEED_DATA" => {
                                // Only decode against a layout the server
                                // agreed to, and against *that* layout rather
                                // than the one this client asked for: the two
                                // are not always the same.
                                let layout = channel
                                    .and_then(|ch| recover(&channel_schemas).get(&ch).cloned());

                                let Some(layout) = layout else {
                                    debug!(
                                        "Dropping FEED_DATA for channel {:?}: no validated layout",
                                        channel
                                    );
                                    continue;
                                };

                                if let Ok(data_msg) =
                                    serde_json::from_str::<FeedDataMessage<Vec<CompactData>>>(&msg)
                                {
                                    // Hand off and keep reading. Delivery is
                                    // somebody else's job: doing it here let
                                    // one slow callback or a full consumer
                                    // stream stop the socket reads that every
                                    // channel operation depends on.
                                    let decoded =
                                        match try_parse_negotiated(&data_msg.data, &layout) {
                                            Ok(events) => events,
                                            Err(e) => {
                                                // Report it rather than
                                                // delivering a partial batch
                                                // that looks complete.
                                                error!(
                                                    "Malformed COMPACT data on channel {:?}: {e}",
                                                    channel
                                                );
                                                continue;
                                            }
                                        };

                                    for event in decoded {
                                        match delivery_tx.try_send(event) {
                                            Ok(()) => {}
                                            Err(mpsc::error::TrySendError::Full(_)) => {
                                                dropped_events += 1;
                                                if dropped_events.is_power_of_two() {
                                                    warn!(
                                                        "Delivery queue full, {} event(s) dropped so far; \
                                                             the consumer is slower than the feed",
                                                        dropped_events
                                                    );
                                                }
                                            }
                                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                                // Delivery has stopped entirely, which is a
                                                // different thing from falling behind and
                                                // must not be reported as backpressure.
                                                error!(
                                                    "Delivery worker is gone; no further events \
                                                         will reach callbacks or the stream"
                                                );
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            "ERROR" => {
                                if let Ok(error_msg) = serde_json::from_str::<ErrorMessage>(&msg) {
                                    error!(
                                        "Received error from server: {} - {}",
                                        error_msg.error, error_msg.message
                                    );
                                }
                            }
                            "KEEPALIVE" => {
                                // Simplemente registrar keepalives
                                debug!("Received KEEPALIVE message");
                            }
                            _ => {
                                debug!("Received unhandled message type: {}", msg_type);
                            }
                        }
                    }
                }
                // The connection is gone: reading the same dead socket again
                // can only fail, so stop instead of spinning forever.
                Err(e) if e.is_terminal() => {
                    error!("Connection lost, stopping message processing: {}", e);
                    record_reason(&disconnect_reason, &e);
                    terminal_reason = Some(e.to_string());
                    break;
                }
                Err(e) => {
                    error!("Error receiving message: {}", e);
                    // A short pause so repeated errors do not flood the logs
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        // This task only ever exits because the session is dead, so the
        // socket is gone: stop writing to it as well.
        if let Some(stop) = shutdown_keepalive {
            let _ = stop.send(()).await;
            debug!("Asked the keepalive task to stop, the session is dead");
        }

        // Tell the supervisor, if there is one. Sent after the keepalive has
        // been asked to stop, so a rebuild never races the old session's writer.
        if let (Some(tx), Some(reason)) = (session_lost, terminal_reason) {
            let _ = tx.send(reason).await;
        }
    })
}

/// Sends `message` and waits for `expected_type` on `channel`, cleaning the
/// registration up on every exit path.
///
/// The registration goes in *before* the send: the reader task is already
/// running, so registering afterwards is a lost-wakeup race. A guard removes it
/// on timeout, on send failure, and if the caller's future is dropped mid-wait,
/// none of which used to clean up.
///
/// Free-standing so a reconnect can rebuild a session — reopen channels,
/// reconfigure feeds, replay subscriptions — without a `&self` it does not have.
#[allow(clippy::too_many_arguments)]
async fn request_response<T: serde::Serialize>(
    connection: &WebSocketConnection,
    response_requests: &Arc<Mutex<Vec<ResponseRequest>>>,
    next_request_id: &Arc<Mutex<u64>>,
    message: &T,
    operation: &str,
    expected_type: &str,
    channel: u32,
    wait: Duration,
) -> DXLinkResult<ResponseType> {
    let (tx, rx) = oneshot::channel();

    let id = {
        let mut next = next_request_id
            .lock()
            .map_err(|_| DXLinkError::Unknown("request id lock poisoned".to_string()))?;
        *next += 1;
        *next
    };

    {
        let mut requests = response_requests
            .lock()
            .map_err(|_| DXLinkError::Unknown("response request lock poisoned".to_string()))?;
        requests.push(ResponseRequest {
            id,
            expected_type: expected_type.to_string(),
            channel_id: Some(channel),
            response_sender: tx,
        });
    }

    let _guard = PendingGuard {
        requests: Arc::clone(response_requests),
        id,
    };

    connection.send(message).await?;

    match tokio::time::timeout(wait, rx).await {
        Ok(Ok(ResponseType::Error(code, message))) => {
            let detail = format!(
                "during {operation} on channel {channel}, waiting for {expected_type}, \
                     the server returned {code}: {message}"
            );
            // Pick the variant from the code: an UNAUTHORIZED here means the
            // same thing it means during the handshake, and collapsing it to
            // Protocol would lose the terminal classification a caller needs
            // to know it must re-authenticate rather than retry.
            Err(match code.to_ascii_uppercase().as_str() {
                "UNAUTHORIZED" => DXLinkError::Authentication(detail),
                // The channel itself is the problem, as opposed to the
                // action, which is a protocol violation and falls through.
                "INVALID_CHANNEL" | "UNKNOWN_CHANNEL" => DXLinkError::Channel(detail),
                "TIMEOUT" => DXLinkError::Timeout(detail),
                _ => DXLinkError::Protocol(detail),
            })
        }
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => Err(DXLinkError::Protocol(format!(
            "during {operation} on channel {channel}, the response channel closed"
        ))),
        Err(_) => Err(DXLinkError::Timeout(format!(
            "timed out after {}s waiting for {expected_type} on channel {channel} during {operation}",
            wait.as_secs()
        ))),
    }
}

/// Opens a socket and takes it through `SETUP` and `AUTH`.
///
/// Returns the live connection and the keepalive deadline the server
/// negotiated, `None` when it did not name one. A zero is treated as "not
/// negotiated" rather than trusted: it would schedule maintenance in a tight
/// loop.
///
/// Every read is bounded and validated, and `connection` is dropped on any
/// failure, so a partial handshake never leaves a half-established socket
/// behind. Free-standing so a reconnect performs exactly the same handshake as
/// the first connect.
async fn handshake(
    url: &str,
    token: &str,
    keepalive_timeout: u32,
) -> DXLinkResult<(WebSocketConnection, Option<u32>)> {
    let connection = WebSocketConnection::connect(url).await?;

    let setup_msg = SetupMessage {
        channel: MAIN_CHANNEL,
        message_type: "SETUP".to_string(),
        keepalive_timeout,
        accept_keepalive_timeout: keepalive_timeout,
        version: DEFAULT_CLIENT_VERSION.to_string(),
    };

    connection.send(&setup_msg).await?;

    let response = expect_handshake_message(&connection, "SETUP", "SETUP", MAIN_CHANNEL).await?;
    let server_setup: ServerSetupMessage = serde_json::from_str(&response)?;
    debug!(
        "Server SETUP: version={:?}, keepaliveTimeout={:?}",
        server_setup.version, server_setup.keepalive_timeout
    );
    let negotiated = server_setup.keepalive_timeout.filter(|t| *t > 0);

    let response =
        expect_handshake_message(&connection, "SETUP", "AUTH_STATE", MAIN_CHANNEL).await?;
    let auth_state: AuthStateMessage = serde_json::from_str(&response)?;

    // Already authorized means the token was accepted on the connection.
    if auth_state.state == "AUTHORIZED" {
        info!("Already authorized to DXLink server");
    } else if auth_state.state == "UNAUTHORIZED" {
        let auth_msg = AuthMessage {
            channel: MAIN_CHANNEL,
            message_type: "AUTH".to_string(),
            token: token.to_string(),
        };

        connection.send(&auth_msg).await?;

        let response =
            expect_handshake_message(&connection, "AUTH", "AUTH_STATE", MAIN_CHANNEL).await?;
        let auth_state: AuthStateMessage = serde_json::from_str(&response)?;

        if auth_state.state != "AUTHORIZED" {
            return Err(DXLinkError::Authentication(format!(
                "during AUTH, the server reported state {} instead of AUTHORIZED",
                auth_state.state
            )));
        }

        info!("Successfully authenticated to DXLink server");
    } else {
        return Err(DXLinkError::Protocol(format!(
            "during SETUP, the server reported an unknown authentication state: {}",
            auth_state.state
        )));
    }

    info!("Successfully connected to DXLink server");

    Ok((connection, negotiated))
}

/// How the client should behave when a live session dies.
///
/// **Reconnection is off unless this is installed** with
/// [`DXLinkClient::with_reconnect`]. The library does not retry behind a
/// consumer's back: a hidden policy is the kind of behaviour that turns one
/// outage into a thundering herd.
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    /// Wait before the first attempt. Doubles each attempt after that.
    pub initial_delay: Duration,
    /// Ceiling for the backoff, so it stops doubling somewhere.
    pub max_delay: Duration,
    /// How many attempts before giving up. `None` retries forever.
    pub max_attempts: Option<u32>,
    /// Spread the delay over `[0, delay]` instead of using it exactly.
    ///
    /// On for good reason: without it, every client an outage knocked over
    /// comes back at the same instant.
    pub jitter: bool,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            max_attempts: None,
            jitter: true,
        }
    }
}

impl ReconnectPolicy {
    /// The delay before `attempt`, counting from 1.
    ///
    /// `seed` is only read when jitter is on; the caller advances it so a run
    /// of attempts does not repeat the same spread.
    fn delay_for(&self, attempt: u32, seed: &mut u64) -> Duration {
        // Saturating rather than wrapping: a long outage must not fold the
        // backoff back round to zero and start hammering.
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1).min(31));
        // Clamped to a floor: zero delays with unlimited attempts would spin.
        let delay = self
            .initial_delay
            .saturating_mul(factor)
            .min(self.max_delay.max(MIN_RECONNECT_DELAY))
            .max(MIN_RECONNECT_DELAY);

        if !self.jitter {
            return delay;
        }

        // xorshift64: enough spread to break up a herd, and no dependency.
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        let nanos = delay.as_nanos() as u64;
        if nanos == 0 {
            return delay;
        }
        // Spread over [0, delay], then back up to the floor: jitter must not
        // reintroduce the tight loop the clamp above just removed.
        Duration::from_nanos(*seed % nanos.saturating_add(1)).max(MIN_RECONNECT_DELAY)
    }
}

/// What happened to the connection.
///
/// Delivered on its own stream rather than through [`MarketEvent`]: a
/// connection event is not market data, and a consumer matching on market
/// events should not have to care that this exists.
///
/// `#[non_exhaustive]`: a future state must not break a downstream match.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionState {
    /// The session died. Carries the reason, the same text
    /// [`DXLinkClient::disconnect_reason`] reports.
    Lost {
        /// Why the session ended.
        reason: String,
    },
    /// About to try again, after waiting `delay`.
    Reconnecting {
        /// Attempt number, counting from 1.
        attempt: u32,
        /// How long the client waited before this attempt.
        delay: Duration,
    },
    /// The session is live again, with every channel and subscription replayed.
    Reconnected,
    /// No more attempts will be made, and the event stream is closing.
    GaveUp {
        /// Why the client stopped trying.
        reason: String,
    },
}

/// The shared state a reconnect needs to rebuild a session.
///
/// Every field is a handle to state the client also holds, so a rebuild is
/// visible to the caller's `subscriptions()` and `disconnect()` without any
/// copying back.
#[derive(Clone)]
struct ReconnectContext {
    url: String,
    token: String,
    keepalive_timeout: u32,
    connection: SharedConnection,
    channels: Arc<Mutex<HashMap<u32, String>>>,
    channel_contracts: Arc<Mutex<HashMap<u32, String>>>,
    channel_schemas: ChannelSchemas,
    subscriptions: Arc<Mutex<SubscriptionBook>>,
    response_requests: Arc<Mutex<Vec<ResponseRequest>>>,
    next_request_id: Arc<Mutex<u64>>,
    disconnect_reason: Arc<Mutex<Option<String>>>,
    delivery_tx: Sender<MarketEvent>,
    /// The slots the rebuilt session's tasks go into, so `disconnect` owns them
    /// rather than the supervisor detaching them.
    session_tasks: SessionTasks,
}

/// Rebuilds a session on the connection that is already installed: reopens
/// every known channel, reconfigures its feed, and replays its subscriptions.
///
/// Order matters. Channels come back before their feeds, feeds before their
/// subscriptions, and subscriptions go out in the order the consumer asked for
/// them, because a server applying them in sequence would otherwise see a
/// different session than the one that was lost.
async fn replay_session(ctx: &ReconnectContext) -> DXLinkResult<()> {
    let connection = live_connection(&ctx.connection)?;

    // Snapshot under the lock, then release it: everything below awaits.
    let known: Vec<(u32, String, String)> = {
        let channels = recover(&ctx.channels);
        let contracts = recover(&ctx.channel_contracts);
        let mut known: Vec<(u32, String, String)> = channels
            .iter()
            .map(|(id, service)| {
                let contract = contracts
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| "AUTO".to_string());
                (*id, service.clone(), contract)
            })
            .collect();
        // Lowest first, so a replay is reproducible rather than hash-ordered.
        known.sort_unstable_by_key(|(id, _, _)| *id);
        known
    };

    for (channel_id, service, contract) in known {
        let mut params = HashMap::new();
        params.insert("contract".to_string(), contract);
        let channel_request = ChannelRequestMessage {
            channel: channel_id,
            message_type: "CHANNEL_REQUEST".to_string(),
            service: service.clone(),
            parameters: params,
        };

        match request_response(
            &connection,
            &ctx.response_requests,
            &ctx.next_request_id,
            &channel_request,
            "reconnect: reopen channel",
            "CHANNEL_OPENED",
            channel_id,
            Duration::from_secs(10),
        )
        .await?
        {
            ResponseType::ChannelOpened(opened) if opened == channel_id => {}
            other => {
                return Err(DXLinkError::Channel(format!(
                    "reopening channel {channel_id} answered with {other:?}"
                )));
            }
        }

        // The layout the channel was configured with. A channel that never got
        // as far as a validated feed has nothing to reconfigure and no
        // subscriptions to replay.
        let stored = recover(&ctx.channel_schemas).get(&channel_id).cloned();
        let Some(stored) = stored else {
            continue;
        };

        // Back through EventType so the request and the validation come from
        // compact_fields, exactly as setup_feed builds them. Names in the store
        // are this client's own, so an unknown one is a bug here rather than
        // bad input.
        let mut event_types: Vec<EventType> = Vec::with_capacity(stored.len());
        for name in stored.keys() {
            let Some(event_type) = EventType::from_wire_name(name) else {
                return Err(DXLinkError::Protocol(format!(
                    "channel {channel_id} was configured for `{name}`, which is not a DXLink \
                     event type"
                )));
            };
            event_types.push(event_type);
        }
        event_types.sort_unstable_by_key(|event_type| event_type.to_string());

        let feed_setup = FeedSetupMessage {
            channel: channel_id,
            message_type: "FEED_SETUP".to_string(),
            accept_aggregation_period: 0.1,
            accept_data_format: "COMPACT".to_string(),
            accept_event_fields: requested_fields(&event_types),
        };

        // Drop the old layout before asking for a new one: a reconfiguration
        // that fails validation must not leave the channel decoding against a
        // contract nobody agreed to.
        recover(&ctx.channel_schemas).remove(&channel_id);

        match request_response(
            &connection,
            &ctx.response_requests,
            &ctx.next_request_id,
            &feed_setup,
            "reconnect: reconfigure feed",
            "FEED_CONFIG",
            channel_id,
            Duration::from_secs(10),
        )
        .await?
        {
            ResponseType::FeedConfig(config) => {
                let schema = negotiated_schema(&config, channel_id, &event_types)?;
                recover(&ctx.channel_schemas).insert(channel_id, schema);
            }
            other => {
                return Err(DXLinkError::Protocol(format!(
                    "reconfiguring channel {channel_id} answered with {other:?}"
                )));
            }
        }

        let to_replay = recover(&ctx.subscriptions).of_channel(channel_id);
        if to_replay.is_empty() {
            continue;
        }

        let subscription_msg = FeedSubscriptionMessage {
            channel: channel_id,
            message_type: "FEED_SUBSCRIPTION".to_string(),
            add: Some(to_replay),
            remove: None,
            reset: None,
        };
        connection.send(&subscription_msg).await?;
    }

    Ok(())
}

/// Runs one reconnect cycle: back off, handshake, rebuild, restart the tasks.
///
/// Returns the handles of the session it established. An error means this
/// attempt failed; whether to try again is the caller's decision, because only
/// it knows the policy and the attempt count.
async fn reconnect_once(
    ctx: &ReconnectContext,
    session_lost: mpsc::Sender<String>,
) -> DXLinkResult<()> {
    // Taken before anything is torn down. `replay_session` clears each
    // channel's layout before asking for a new one, so a failure partway
    // through would otherwise leave the next attempt with nothing to
    // reconfigure and no way to know it was ever configured.
    let schemas_before = recover(&ctx.channel_schemas).clone();

    let (connection, negotiated) = handshake(&ctx.url, &ctx.token, ctx.keepalive_timeout).await?;
    *recover(&ctx.connection) = Some(connection.clone());

    // The reader has to be up before the rebuild sends anything, because it is
    // the only thing that routes the replies the rebuild waits on.
    let interval = keepalive_interval_for(ctx.keepalive_timeout, negotiated);
    let (keepalive_handle, keepalive_stop) = spawn_keepalive(connection.clone(), interval);
    let reader = spawn_reader(ReaderSetup {
        connection,
        shutdown_keepalive: Some(keepalive_stop.clone()),
        delivery_tx: ctx.delivery_tx.clone(),
        disconnect_reason: ctx.disconnect_reason.clone(),
        response_requests: ctx.response_requests.clone(),
        channel_schemas: ctx.channel_schemas.clone(),
        receive_deadline: Duration::from_secs(u64::from(ctx.keepalive_timeout)),
        session_lost: Some(session_lost),
    });

    match replay_session(ctx).await {
        Ok(()) => {
            // Into the shared slots, never dropped here: a dropped JoinHandle
            // detaches its task, which would leave disconnect owning only the
            // dead original session and unable to stop this one.
            ctx.session_tasks
                .install(reader, keepalive_handle, keepalive_stop);
            Ok(())
        }
        Err(e) => {
            // A half-rebuilt session is worse than none: it would deliver some
            // channels and silently miss others. Tear it down and let the
            // caller decide whether to try again.
            let _ = keepalive_stop.send(()).await;
            reader.abort();
            keepalive_handle.abort();
            *recover(&ctx.connection) = None;
            // The layouts this attempt cleared have to come back, or the next
            // attempt sees no schema, skips feed setup and subscriptions, and
            // reports success having only opened the channels.
            *recover(&ctx.channel_schemas) = schemas_before;
            Err(e)
        }
    }
}

/// How often to send maintenance, given what we advertised and what the server
/// negotiated.
///
/// Free-standing because a reconnect has to derive the same interval from the
/// value the new handshake negotiated, without a `&self`.
fn keepalive_interval_for(advertised: u32, negotiated: Option<u32>) -> Duration {
    let deadline = negotiated.unwrap_or(advertised);
    Duration::from_secs(u64::from(deadline.div_ceil(KEEPALIVE_DIVISOR))).max(MIN_KEEPALIVE_INTERVAL)
}

/// Reports a connection-state change, never blocking the supervisor.
///
/// A full queue **evicts the oldest state**, so a consumer that falls behind
/// loses the middle of a reconnect rather than its end. That is the right way
/// round: `Reconnecting { attempt: 3 }` is worth less than the `Reconnected` or
/// `GaveUp` that follows it, and the previous `mpsc` did the opposite because a
/// sender cannot evict from the front of its own queue.
///
/// A send with no subscribers is not a failure, it is the normal case for a
/// consumer that never asked for the stream — and it is the case on **every**
/// transition, so it is dropped silently rather than logged.
fn notify(states: &Option<broadcast::Sender<ConnectionState>>, state: ConnectionState) {
    if let Some(tx) = states {
        let _ = tx.send(state);
    }
}

/// Whether a failed reconnect attempt is worth repeating.
///
/// Deliberately **not** [`DXLinkError::is_terminal`]. That answers "can this
/// socket still be used", which is a different question: a `Timeout` is not
/// terminal there, because one unanswered read does not condemn a live
/// connection — but a peer that accepts the reconnect and then never answers
/// `SETUP` or `FEED_CONFIG` is exactly the case reconnection exists for, and
/// treating it as unretryable gave up after a single attempt regardless of the
/// policy's limit.
///
/// Matched exhaustively, with no `_` arm, so a new error variant forces this
/// decision rather than defaulting to one.
fn worth_retrying(error: &DXLinkError) -> bool {
    match error {
        // The transport failed or the peer went quiet. What reconnection is for.
        DXLinkError::Connection(_)
        | DXLinkError::WebSocket(_)
        | DXLinkError::Timeout(_)
        | DXLinkError::Channel(_) => true,
        // The token will be rejected just as fast the second time.
        DXLinkError::Authentication(_) => false,
        // We and the server disagree about the protocol, or this client has a
        // bug. Either way the next attempt reproduces it, and hammering a
        // server over a disagreement is worse than stopping and saying so.
        DXLinkError::Protocol(_)
        | DXLinkError::Serialization(_)
        | DXLinkError::UnexpectedMessage(_)
        | DXLinkError::Unknown(_) => false,
    }
}

/// Locks a client-state mutex, recovering rather than panicking if it is
/// poisoned.
///
/// A poisoned lock means another thread panicked while holding it. Everything
/// behind these locks is plain bookkeeping — channel ids, subscriptions,
/// callbacks — not an invariant a partial write could corrupt, so turning one
/// panic into a second panic on every later request helps nobody. Recovering
/// keeps the client usable and leaves the original panic to be reported where
/// it happened.
fn recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Field layouts, per event type, that a channel was configured with.
type ChannelSchema = HashMap<String, Vec<String>>;

/// Validated layouts per channel.
type ChannelSchemas = Arc<Mutex<HashMap<u32, ChannelSchema>>>;

/// The tasks that belong to one session, in slots a reconnect can swap.
///
/// Shared because a reconnect replaces the reader and the keepalive, and
/// `disconnect` has to be able to stop **whichever** session is live — not the
/// dead one it happened to spawn itself. Dropping a `JoinHandle` detaches the
/// task rather than stopping it, so a handle that is not in one of these slots
/// is a task nothing can ever kill.
#[derive(Clone, Default)]
struct SessionTasks {
    reader: Arc<Mutex<Option<JoinHandle<()>>>>,
    keepalive: Arc<Mutex<Option<JoinHandle<()>>>>,
    keepalive_stop: Arc<Mutex<Option<mpsc::Sender<()>>>>,
}

impl SessionTasks {
    /// Installs a session's tasks, aborting whatever was in the slots.
    ///
    /// The previous session is dead by the time a reconnect gets here, but
    /// aborting is what makes that true rather than assumed.
    fn install(&self, reader: JoinHandle<()>, keepalive: JoinHandle<()>, stop: mpsc::Sender<()>) {
        if let Some(previous) = recover(&self.reader).replace(reader) {
            previous.abort();
        }
        if let Some(previous) = recover(&self.keepalive).replace(keepalive) {
            previous.abort();
        }
        *recover(&self.keepalive_stop) = Some(stop);
    }

    /// The channel that stops the live keepalive, if there is one.
    fn keepalive_stop(&self) -> Option<mpsc::Sender<()>> {
        recover(&self.keepalive_stop).clone()
    }

    /// Aborts both tasks and forgets them. Safe to call more than once.
    fn abort(&self) {
        if let Some(reader) = recover(&self.reader).take() {
            reader.abort();
        }
        if let Some(keepalive) = recover(&self.keepalive).take() {
            keepalive.abort();
        }
        *recover(&self.keepalive_stop) = None;
    }
}

/// The live socket, in a slot a background task can swap.
///
/// Shared rather than owned so a reconnect can replace the connection under the
/// methods that use it. The guard is only ever held long enough to clone the
/// handle out — `WebSocketConnection` is itself `Arc`-backed — so it never
/// crosses an `.await`, which is the rule for every `std::sync::Mutex` in this
/// file.
type SharedConnection = Arc<Mutex<Option<WebSocketConnection>>>;

/// One tracked subscription, with the order it was asked for.
///
/// The order is what makes replay reproducible: a map alone would hand the
/// subscriptions back in an arbitrary order, and a server that applies them in
/// sequence would see a different session than the consumer built.
#[derive(Debug, Clone)]
struct TrackedSubscription {
    order: u64,
    subscription: FeedSubscription,
}

/// What identifies a subscription inside one channel: event type, symbol and
/// source.
///
/// `source` is part of the identity because an indexed subscription is scoped
/// to it — the same type and symbol from two sources are two subscriptions, and
/// removing one must not forget the other. `fromTime` is **not**: it is a
/// parameter of the series, so resubscribing the same symbol from the same
/// source with a new time replaces the entry rather than adding a second one.
type SubscriptionKey = (EventType, String, Option<String>);

/// Live subscriptions, per channel, keyed the way the server keys them.
///
/// Subscribing twice to the same [`SubscriptionKey`] on one channel
/// **replaces** the entry: the second call is what the server ends up
/// honouring, and tracking both would make a replay send a subscription the
/// server had already superseded.
///
/// Per channel, because two feed channels can legitimately hold the same event
/// and symbol with different aggregation, and closing or resetting one must not
/// forget the other's.
#[derive(Debug, Default)]
struct SubscriptionBook {
    by_channel: HashMap<u32, HashMap<SubscriptionKey, TrackedSubscription>>,
    next_order: u64,
}

/// The identity of a subscription, for tracking it.
fn key_of(event_type: EventType, subscription: &FeedSubscription) -> SubscriptionKey {
    (
        event_type,
        subscription.symbol.clone(),
        subscription.source.clone(),
    )
}

impl SubscriptionBook {
    /// Records a subscription, replacing any earlier one with the same identity
    /// on that channel.
    fn insert(&mut self, channel_id: u32, event_type: EventType, subscription: FeedSubscription) {
        let order = self.next_order;
        self.next_order += 1;
        let key = key_of(event_type, &subscription);
        self.by_channel.entry(channel_id).or_default().insert(
            key,
            TrackedSubscription {
                order,
                subscription,
            },
        );
    }

    /// Forgets one subscription. Unknown entries are ignored: the server treats
    /// removing something that is not there as a no-op too.
    fn remove(&mut self, channel_id: u32, event_type: EventType, subscription: &FeedSubscription) {
        if let Some(channel) = self.by_channel.get_mut(&channel_id) {
            channel.remove(&key_of(event_type, subscription));
            if channel.is_empty() {
                self.by_channel.remove(&channel_id);
            }
        }
    }

    /// Forgets everything on one channel, leaving every other channel alone.
    fn forget_channel(&mut self, channel_id: u32) {
        self.by_channel.remove(&channel_id);
    }

    /// The live subscriptions on a channel, in the order they were asked for.
    fn of_channel(&self, channel_id: u32) -> Vec<FeedSubscription> {
        let Some(channel) = self.by_channel.get(&channel_id) else {
            return Vec::new();
        };
        let mut tracked: Vec<&TrackedSubscription> = channel.values().collect();
        tracked.sort_unstable_by_key(|entry| entry.order);
        tracked
            .iter()
            .map(|entry| entry.subscription.clone())
            .collect()
    }

    /// Every channel that has at least one subscription, lowest id first.
    fn channels(&self) -> Vec<u32> {
        let mut channels: Vec<u32> = self.by_channel.keys().copied().collect();
        channels.sort_unstable();
        channels
    }

    /// How many subscriptions are live across every channel.
    fn len(&self) -> usize {
        self.by_channel.values().map(HashMap::len).sum()
    }

    fn clear(&mut self) {
        self.by_channel.clear();
    }
}

/// The COMPACT field lists this client requests for a set of event types.
/// Callers reach this only after refusing types with no decoder, so a type
/// without one is skipped rather than given the two-field fallback that made
/// the old half-configured behaviour possible.
fn requested_fields(event_types: &[EventType]) -> ChannelSchema {
    event_types
        .iter()
        .filter_map(|event_type| {
            let fields = event_type
                .compact_fields()?
                .iter()
                .map(|f| (*f).to_string())
                .collect();
            Some((event_type.to_string(), fields))
        })
        .collect()
}

/// The layout a channel will actually be decoded against.
///
/// `FEED_CONFIG` reports the **actual** configuration, which is not always the
/// one requested: a server may serve a subset of the fields asked for, and the
/// dxFeed demo does exactly that, dropping `VWAP` from `Candle`. So the reply is
/// adopted rather than merely checked. What it cannot do is change the data
/// format, or leave out the two columns that identify a row.
///
/// A type the server did not mention keeps the requested list, which is the
/// spec's own reading: silence is agreement.
fn negotiated_schema(
    config: &FeedConfigMessage,
    channel_id: u32,
    event_types: &[EventType],
) -> DXLinkResult<ChannelSchema> {
    if !config.data_format.eq_ignore_ascii_case("COMPACT") {
        return Err(DXLinkError::Protocol(format!(
            "channel {channel_id}: this client decodes COMPACT rows, but the server \
             negotiated {}",
            config.data_format
        )));
    }

    let mut schema = requested_fields(event_types);
    let Some(negotiated) = config.event_fields.as_ref() else {
        return Ok(schema);
    };

    for (event_type, fields) in negotiated {
        // Only what was asked for. A server volunteering a type we never
        // requested has nothing to deliver on this channel anyway.
        if !schema.contains_key(event_type) {
            continue;
        }
        check_identifiable(event_type, channel_id, fields)?;
        schema.insert(event_type.clone(), fields.clone());
    }

    Ok(schema)
}

/// Takes on the layout a `FEED_CONFIG` reports, for a channel already
/// configured.
///
/// Only the types the channel knows about, and only layouts whose rows can
/// still be identified. A channel with no stored schema is one that was never
/// set up, so there is nothing to update.
fn adopt_config(schemas: &ChannelSchemas, channel_id: u32, config: &FeedConfigMessage) {
    if !config.data_format.eq_ignore_ascii_case("COMPACT") {
        if recover(schemas).remove(&channel_id).is_some() {
            error!(
                "Channel {channel_id} was moved to {} data, which this client cannot \
                 decode; its data will be dropped",
                config.data_format
            );
        }
        return;
    }

    let Some(negotiated) = config.event_fields.as_ref() else {
        return;
    };

    let mut schemas = recover(schemas);
    let Some(stored) = schemas.get_mut(&channel_id) else {
        return;
    };

    for (event_type, fields) in negotiated {
        let Some(known) = stored.get(event_type) else {
            continue;
        };
        if known == fields {
            continue;
        }
        if let Err(e) = check_identifiable(event_type, channel_id, fields) {
            debug!("Ignoring an unusable {event_type} layout on channel {channel_id}: {e}");
            continue;
        }
        info!(
            "Channel {channel_id} negotiated a different {event_type} layout; decoding \
             against it. Fields this client asked for and will not get: {:?}",
            known
                .iter()
                .filter(|field| !fields.contains(field))
                .collect::<Vec<_>>()
        );
        stored.insert(event_type.clone(), fields.clone());
    }
}

/// Rejects a layout that cannot identify its own rows.
///
/// Everything else may be missing and read as "not provided", but without these
/// two there is no way to tell what a row is or what it is about, and decoding
/// it would be guessing.
fn check_identifiable(event_type: &str, channel_id: u32, fields: &[String]) -> DXLinkResult<()> {
    for required in ["eventType", "eventSymbol"] {
        if !fields.iter().any(|field| field == required) {
            return Err(DXLinkError::Protocol(format!(
                "channel {channel_id}: the server negotiated a {event_type} layout with no \
                 `{required}` column ({fields:?}), so its rows cannot be identified"
            )));
        }
    }
    Ok(())
}

/// The event types this client can turn into a [`MarketEvent`], for error text.
fn decodable_type_names() -> String {
    ALL_EVENT_TYPES
        .iter()
        .filter(|event_type| event_type.compact_fields().is_some())
        .map(|event_type| event_type.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parses every subscription's event type, rejecting the whole batch if any is
/// unknown.
///
/// All or nothing on purpose: sending half a batch and recording the other half
/// leaves the client and the server disagreeing about what is subscribed, which
/// is worse than refusing the call.
fn parse_subscription_types(
    subscriptions: &[FeedSubscription],
    channel_id: u32,
    configured: &ChannelSchema,
) -> DXLinkResult<Vec<EventType>> {
    subscriptions
        .iter()
        .map(|sub| {
            let event_type = sub.event_type.parse::<EventType>()?;

            if event_type.compact_fields().is_none() {
                return Err(DXLinkError::Protocol(format!(
                    "this client cannot decode {event_type} events, so subscribing `{}` to \
                     them would return nothing. Decoded types: {}",
                    sub.symbol,
                    decodable_type_names()
                )));
            }

            // Decodable is not enough: the layout has to be one this channel
            // negotiated. Subscribing Trade on a Quote-configured channel used
            // to pass, and the reader's channel-level gate then decoded Trade
            // rows against a layout that was never agreed.
            if !configured.contains_key(&event_type.to_string()) {
                return Err(DXLinkError::Protocol(format!(
                    "channel {channel_id} was not configured for {event_type}; call \
                     setup_feed with it before subscribing `{}`. Configured: {}",
                    sub.symbol,
                    configured.keys().cloned().collect::<Vec<_>>().join(", ")
                )));
            }

            Ok(event_type)
        })
        .collect()
}

/// The symbol an event refers to, borrowed rather than cloned.
fn symbol_of(event: &MarketEvent) -> &str {
    match event {
        MarketEvent::Quote(e) => &e.event_symbol,
        MarketEvent::Trade(e) => &e.event_symbol,
        MarketEvent::Greeks(e) => &e.event_symbol,
        MarketEvent::Candle(e) => &e.event_symbol,
        MarketEvent::Summary(e) => &e.event_symbol,
        MarketEvent::TimeAndSale(e) => &e.event_symbol,
        MarketEvent::Profile(e) => &e.event_symbol,
        MarketEvent::Underlying(e) => &e.event_symbol,
        MarketEvent::TheoPrice(e) => &e.event_symbol,
    }
}

/// Records why a session ended, keeping the first reason rather than the last.
///
/// The first failure is the cause; anything after it is a consequence of a
/// connection that had already gone.
fn record_reason(slot: &Arc<Mutex<Option<String>>>, error: &DXLinkError) {
    let mut slot = recover(slot);
    if slot.is_none() {
        *slot = Some(error.to_string());
    }
}

/// Removes a pending response registration when it goes out of scope.
///
/// Registrations used to survive timeouts, cancelled futures and failed sends.
/// A stale entry then matched the next valid response and consumed it, so a
/// live waiter timed out instead. Tying removal to `Drop` covers every exit
/// path, including the caller's future simply being dropped.
struct PendingGuard {
    requests: Arc<Mutex<Vec<ResponseRequest>>>,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        recover(&self.requests).retain(|request| request.id != self.id);
    }
}

/// Represents a client for interacting with the DXLink service.
///
/// The `DXLinkClient` provides methods for connecting to a DXLink WebSocket server,
/// subscribing to market data feeds, and receiving real-time market events.
///
/// # Fields
///
/// * `url`: The URL of the DXLink WebSocket server.
/// * `token`: The authentication token for accessing the DXLink service.
/// * `connection`: The active WebSocket connection, if established.  This is represented
///   as an `Option<WebSocketConnection>`, where `None` indicates no active connection.
/// * `keepalive_timeout`: The timeout for keepalive messages in seconds.
/// * `next_channel_id`: A thread-safe counter for generating unique channel IDs.  It's
///   wrapped in an `Arc<Mutex>` to allow shared access across multiple threads.
/// * `channels`: A thread-safe map that stores the association between channel IDs and
///   the services they are subscribed to.  This is also wrapped in an `Arc<Mutex>`
///   for thread safety.
/// * `callbacks`: A thread-safe map that stores callback functions associated with
///   specific market data symbols.  The callbacks are of type `EventCallback`,
///   which are functions that process incoming `MarketEvent` data.  An `Arc<Mutex>`
///   is used for thread safety.
/// * `subscriptions`: The live subscriptions, tracked per channel and keyed by
///   event type and symbol, carrying `fromTime` and `source` so a session can be
///   replayed exactly. Committed only after the outbound send succeeds, so a
///   failed send never leaves the client believing in state the server does not
///   have. It uses `Arc<Mutex>` for thread safety.
/// * `event_sender`: A sender for transmitting `MarketEvent` instances.  This is
///   optional (`Option<Sender<MarketEvent>>`) and is used to relay events to
///   internal processing or external consumers.
/// * `keepalive_handle`: A handle to the keepalive task.  The keepalive task
///   periodically sends messages to the server to maintain the connection.
///   This is an `Option<JoinHandle<()>>` which represents a potentially running
///   background task.
/// * `message_handle`: A handle to the message processing task. The message
///   processing task is responsible for receiving and handling incoming WebSocket
///   messages.  This is stored as an `Option<JoinHandle<()>>` to manage the
///   background task's lifecycle.
/// * `keepalive_sender`:  A channel sender used to signal the keepalive task.
///   This is of type `Option<Sender<()>>`, which may be used to control
///   or stop the keepalive task.
/// * `response_requests`: A thread-safe vector that holds pending response requests.
///   This is used to manage asynchronous responses from the server and is wrapped
///   in an `Arc<Mutex>` for thread safety.
pub struct DXLinkClient {
    /// The URL of the DXLink WebSocket server.
    url: String,
    /// The authentication token for accessing the DXLink service.
    token: String,
    /// The active WebSocket connection, if established. `None` indicates no
    /// active connection. Shared so a reconnect task can replace it.
    connection: SharedConnection,
    /// The keepalive timeout this client advertises, in seconds. Also the
    /// deadline after which inbound silence is treated as a dead connection.
    keepalive_timeout: u32,
    /// The keepalive timeout the server asked for, learned from its `SETUP`.
    /// `None` until the handshake completes or if the server did not send one.
    server_keepalive_timeout: Option<u32>,
    /// A thread-safe counter for generating unique channel IDs.
    next_channel_id: Arc<Mutex<u32>>,
    /// A thread-safe map storing the association between channel IDs and the services they are subscribed to.
    channels: Arc<Mutex<HashMap<u32, String>>>, // channel_id -> service
    /// A thread-safe map storing callback functions associated with specific market data symbols.
    callbacks: Arc<Mutex<HashMap<String, Arc<EventCallback>>>>, // symbol -> callback
    /// Live subscriptions, tracked per channel and committed only after the
    /// outbound send succeeds.
    subscriptions: Arc<Mutex<SubscriptionBook>>,
    /// A sender for transmitting `MarketEvent` instances.
    event_sender: Option<Sender<MarketEvent>>,
    /// A handle to the keepalive task.
    session_tasks: SessionTasks,
    /// A handle to the task that runs callbacks and feeds the event stream.
    delivery_handle: Option<JoinHandle<()>>,
    /// Set once the event stream has been handed out, so it cannot be taken
    /// twice even after the sender has moved into the delivery worker.
    event_stream_taken: bool,
    /// Why the session ended, once it has. Shared with the reader, which is
    /// where the terminal error is observed.
    disconnect_reason: Arc<Mutex<Option<String>>>,
    /// A channel sender used to signal the keepalive task.
    /// A thread-safe vector that holds pending response requests.
    response_requests: Arc<Mutex<Vec<ResponseRequest>>>,
    /// Hands out the identity each pending request is cleaned up by.
    next_request_id: Arc<Mutex<u64>>,
    /// The field layout validated for each configured channel. A channel with
    /// no entry has no agreed layout, so its rows must not be decoded.
    channel_schemas: ChannelSchemas,
    /// The contract each channel was opened with, so a replay reopens it the
    /// same way rather than guessing.
    channel_contracts: Arc<Mutex<HashMap<u32, String>>>,
    /// The reconnection policy, if the consumer installed one. `None` is the
    /// default and means the client never retries.
    reconnect: Option<ReconnectPolicy>,
    /// Broadcasts connection-state changes. `None` until a session with a
    /// reconnect policy is established, and dropped again on `disconnect`, which
    /// is what closes a consumer's stream.
    state_sender: Option<broadcast::Sender<ConnectionState>>,
    /// A handle to the reconnect supervisor, when one is running.
    supervisor_handle: Option<JoinHandle<()>>,
    /// Stops the supervisor, including a backoff it is in the middle of.
    supervisor_shutdown: Option<mpsc::Sender<()>>,
    /// Handed to each reader so it can report a terminal failure once.
    session_lost_sender: Option<mpsc::Sender<String>>,
    /// The receiving half, until the supervisor takes it.
    session_lost_receiver: Option<mpsc::Receiver<String>>,
    /// Hand-off slot for the delivery queue, between spawning the reader and
    /// spawning the supervisor.
    ///
    /// **Taken, never held.** The delivery worker stops when its last sender
    /// drops, and that is what closes the consumer's stream — a client keeping
    /// a clone would hold the stream open forever over a dead socket.
    pending_delivery_tx: Option<Sender<MarketEvent>>,
}

impl DXLinkClient {
    /// Creates a new instance of the `DXLinkClient`.
    ///
    /// This function initializes a new `DXLinkClient` with the provided URL and token.  The client is not connected
    /// to the server at this point; a separate call to the `connect` method is required to establish a connection.
    ///
    /// # Arguments
    ///
    /// * `url`: The URL of the DXLink WebSocket server.  This should be a valid WebSocket URL.
    /// * `token`: The authentication token required to access the DXLink service.
    ///
    /// # Returns
    ///
    /// A new instance of the `DXLinkClient`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use dxlink::DXLinkClient;
    /// let client = DXLinkClient::new("wss://example.com/dxlink", "YOUR_TOKEN");
    /// ```
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            url: url.to_string(),
            token: token.to_string(),
            connection: Arc::new(Mutex::new(None)),
            keepalive_timeout: DEFAULT_KEEPALIVE_TIMEOUT,
            server_keepalive_timeout: None,
            next_channel_id: Arc::new(Mutex::new(1)), // Start from 1 as 0 is the main channel
            channels: Arc::new(Mutex::new(HashMap::new())),
            callbacks: Arc::new(Mutex::new(HashMap::new())),
            subscriptions: Arc::new(Mutex::new(SubscriptionBook::default())),
            event_sender: None,
            session_tasks: SessionTasks::default(),
            delivery_handle: None,
            event_stream_taken: false,
            disconnect_reason: Arc::new(Mutex::new(None)),
            response_requests: Arc::new(Mutex::new(Vec::new())),
            next_request_id: Arc::new(Mutex::new(0)),
            channel_schemas: Arc::new(Mutex::new(HashMap::new())),
            channel_contracts: Arc::new(Mutex::new(HashMap::new())),
            reconnect: None,
            state_sender: None,
            supervisor_handle: None,
            supervisor_shutdown: None,
            session_lost_sender: None,
            session_lost_receiver: None,
            pending_delivery_tx: None,
        }
    }

    /// Sends `message` and waits for `expected_type` on `channel`, cleaning the
    /// registration up on every exit path.
    ///
    /// The registration goes in *before* the send: the message task is already
    /// running, so registering afterwards is a lost-wakeup race. A guard removes
    /// it on timeout, on send failure, and if the caller's future is dropped
    /// mid-wait, none of which used to clean up.
    async fn request_response<T: serde::Serialize>(
        &self,
        message: &T,
        operation: &str,
        expected_type: &str,
        channel: u32,
        wait: Duration,
    ) -> DXLinkResult<ResponseType> {
        request_response(
            &self.get_connection()?,
            &self.response_requests,
            &self.next_request_id,
            message,
            operation,
            expected_type,
            channel,
            wait,
        )
        .await
    }

    /// A handle to the live connection.
    ///
    /// Cloned out of the slot rather than borrowed: the clone is cheap, and
    /// holding the lock while sending would block a reconnect swapping the
    /// socket underneath.
    fn get_connection(&self) -> DXLinkResult<WebSocketConnection> {
        live_connection(&self.connection)
    }

    /// How many response registrations are currently pending.
    ///
    /// Exposed so a test can assert that a timed-out or cancelled request left
    /// nothing behind that could consume someone else's response.
    pub fn pending_response_count(&self) -> usize {
        self.response_requests
            .lock()
            .map(|requests| requests.len())
            .unwrap_or(0)
    }

    /// Spawns the worker that runs consumer callbacks and feeds the event
    /// stream, and returns the queue the socket reader hands events to.
    ///
    /// Separate from the reader on purpose. Callbacks are user code: they can be
    /// slow, they can panic, and the consumer's stream can fill up. Any of those
    /// happening on the reader stopped it answering `FEED_CONFIG`,
    /// `CHANNEL_CLOSED` and `ERROR`, so an unrelated channel operation timed out
    /// because somebody's callback was busy.
    ///
    /// Backpressure policy: the queue is bounded and a full queue **drops the
    /// event**, counting and logging the loss. Market data is only useful while
    /// it is current, so blocking the reader to preserve a stale quote is the
    /// wrong trade.
    fn start_event_delivery(&mut self) -> Sender<MarketEvent> {
        let (delivery_tx, mut delivery_rx) = mpsc::channel::<MarketEvent>(DELIVERY_QUEUE_CAPACITY);

        let callbacks = self.callbacks.clone();
        // Taken, not cloned: the worker becomes the only owner, so when it ends
        // the channel closes and the consumer's recv() returns None. That is the
        // signal that the session is over.
        let event_sender = self.event_sender.take();

        let handle = tokio::spawn(async move {
            let mut stream_closed_reported = false;

            while let Some(event) = delivery_rx.recv().await {
                // Borrowed, not cloned: this runs per event on a hot path.
                let symbol = symbol_of(&event);

                // Copy the handle out and release the lock before running user
                // code: holding it across a callback serialises every other
                // symbol behind whatever that callback is doing.
                let callback = recover(&callbacks).get(symbol).cloned();

                if let Some(callback) = callback {
                    let delivered = event.clone();
                    // Run it on the blocking pool, not here. A callback that
                    // blocks the thread would otherwise stall this task's
                    // executor thread, and on a current-thread runtime that is
                    // the same thread the protocol reader needs. spawn_blocking
                    // also turns a panic into a JoinError instead of unwinding
                    // through the task.
                    if tokio::task::spawn_blocking(move || callback(delivered))
                        .await
                        .is_err()
                    {
                        error!("Callback for {} panicked; continuing delivery", symbol);
                    }
                }

                if let Some(tx) = &event_sender {
                    match tx.try_send(event) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(returned)) => {
                            // try_send hands the event back, so the symbol is
                            // still available without having cloned it up front.
                            debug!(
                                "Event stream full, dropping an event for {}",
                                symbol_of(&returned)
                            );
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            // The consumer dropped its receiver. Callbacks still
                            // fire; the stream cannot be re-taken, which is what
                            // event_stream documents.
                            if !stream_closed_reported {
                                debug!(
                                    "Event stream receiver dropped; delivering to callbacks only"
                                );
                                stream_closed_reported = true;
                            }
                        }
                    }
                }
            }

            debug!("Event delivery task terminated");
        });

        self.delivery_handle = Some(handle);
        // Parked for the supervisor, which is spawned right after the reader
        // and takes it. A rebuilt session feeds this same worker: it owns the
        // consumer's sender, so restarting it would close the stream the
        // reconnect exists to preserve.
        self.pending_delivery_tx = Some(delivery_tx.clone());
        delivery_tx
    }

    /// Turns on reconnection, with the policy given.
    ///
    /// Off by default and never enabled implicitly. Call this **before**
    /// [`connect`](Self::connect): a policy installed afterwards has nothing to
    /// supervise, because the supervisor is spawned as part of connecting.
    ///
    /// The retry classification is deliberate. Only a terminal connection
    /// failure triggers a reconnect: a dead socket or inbound silence past the
    /// keepalive deadline. An authentication rejection is **not** retried,
    /// because the token will be rejected just as fast the second time, and a
    /// local protocol or configuration error is not retried either.
    ///
    /// Additive: a new method, no existing signature changes.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use dxlink::{DXLinkClient, ReconnectPolicy};
    /// use std::time::Duration;
    ///
    /// let mut client = DXLinkClient::new("wss://example.com/dxlink-ws", "token");
    /// client.with_reconnect(ReconnectPolicy {
    ///     max_attempts: Some(5),
    ///     ..ReconnectPolicy::default()
    /// });
    /// ```
    pub fn with_reconnect(&mut self, policy: ReconnectPolicy) {
        self.reconnect = Some(policy);
        // Opened here rather than in `connect`, so `connection_states` works the
        // moment a policy exists. A `&self` accessor that answered `None` until
        // some other call had happened would be a trap.
        if self.state_sender.is_none() {
            // Normally already open, from `with_reconnect`. This covers a
            // reconnect after a `disconnect`, which drops the old one.
            if self.state_sender.is_none() {
                let (state_tx, _) =
                    broadcast::channel::<ConnectionState>(CONNECTION_STATE_CAPACITY);
                self.state_sender = Some(state_tx);
            }
        }
    }

    /// Subscribes to connection-state changes.
    ///
    /// `None` without a reconnect policy: the session never comes back, and the
    /// closing event stream already says so. With one, every call returns a new
    /// receiver, from [`with_reconnect`](Self::with_reconnect) onwards — no need
    /// to connect first.
    ///
    /// Each receiver sees only what is sent **after** it subscribes, so
    /// subscribe before the states you care about. Subscribing right after
    /// installing the policy is the way to be sure of catching all of them.
    ///
    /// The stream is bounded and never blocks the supervisor. A consumer that
    /// falls behind **loses the oldest states**, is told how many with
    /// [`RecvError::Lagged`](tokio::sync::broadcast::error::RecvError::Lagged),
    /// and carries on — so it can miss the middle of a long reconnect but not
    /// the `Reconnected` or `GaveUp` that ends it.
    ///
    /// The stream closes when the session does: after
    /// [`disconnect`](Self::disconnect), or when the client is dropped.
    /// Subscribing again after a later `connect` gives a live stream.
    pub fn connection_states(&self) -> Option<broadcast::Receiver<ConnectionState>> {
        self.state_sender.as_ref().map(broadcast::Sender::subscribe)
    }

    /// Sets the keepalive timeout this client advertises, in seconds.
    ///
    /// This is the deadline the client asks the server to respect, and the one
    /// after which inbound silence is reported as a dead connection. It does not
    /// change how often the client sends maintenance: that follows what the
    /// server negotiates in its `SETUP` reply.
    ///
    /// Must be called before [`connect`](Self::connect); afterwards it has no
    /// effect on the running session. A zero is rejected.
    ///
    /// # Example
    ///
    /// ```rust
    /// use dxlink::DXLinkClient;
    ///
    /// let client = DXLinkClient::new("wss://example.com", "token")
    ///     .with_keepalive_timeout(30)
    ///     .expect("30 seconds is a valid timeout");
    /// ```
    pub fn with_keepalive_timeout(mut self, seconds: u32) -> DXLinkResult<Self> {
        if seconds == 0 {
            return Err(DXLinkError::Protocol(
                "keepalive timeout must be greater than zero".to_string(),
            ));
        }
        self.keepalive_timeout = seconds;
        Ok(self)
    }

    /// The interval at which maintenance is sent, derived from the negotiated
    /// deadline. Falls back to the advertised timeout when the server did not
    /// negotiate one.
    fn keepalive_interval(&self) -> Duration {
        keepalive_interval_for(self.keepalive_timeout, self.server_keepalive_timeout)
    }

    /// Establishes a connection to the DXLink server.
    ///
    /// This function performs the following steps to connect to the server:
    ///
    /// 1. **Connects to WebSocket:** Establishes a WebSocket connection to the URL specified in the `self.url` field.
    /// 2. **Sends SETUP Message:** Sends a `SetupMessage` to the server, initiating the setup process.  This message includes the channel, message type, keepalive timeout, and client version.
    /// 3. **Receives SETUP Response:** Waits for and receives a `SetupMessage` response from the server, confirming the setup parameters.
    /// 4. **Receives AUTH_STATE Message:** Receives an `AuthStateMessage` to check the current authentication status.
    /// 5. **Handles Authentication:**
    ///    - If the `AuthStateMessage` indicates "AUTHORIZED", the client is already authorized and no further action is taken.
    ///    - If the `AuthStateMessage` indicates "UNAUTHORIZED", the client sends an `AuthMessage` containing the authentication token. It then waits for an `AuthStateMessage` response and checks if the state has changed to "AUTHORIZED".  If not, an authentication error is returned.
    ///    - If the `AuthStateMessage` indicates an unexpected state, a protocol error is returned.
    /// 6. **Starts Message Processing:**  Starts a separate task to handle incoming messages from the server.
    /// 7. **Starts Keepalive:** Starts a keepalive task to maintain the connection by sending periodic keepalive messages.
    ///
    /// # Errors
    ///
    /// This function can return several errors:
    ///
    /// * `DXLinkError::WebSocket`: If there is an error establishing or maintaining the WebSocket connection.
    /// * `DXLinkError::Serialization`: If there is an error serializing or deserializing messages.
    /// * `DXLinkError::Authentication`: If the authentication process fails.
    /// * `DXLinkError::Protocol`: If an unexpected message or state is encountered during the connection process.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use dxlink::{DXLinkClient, DXLinkError};
    ///
    /// # async fn example() -> Result<(), DXLinkError> {
    /// let mut client = DXLinkClient::new("wss://your_dxlink_server_url", "YOUR_TOKEN");
    /// // `connect` returns the event stream; there is exactly one per client.
    /// let mut event_stream = client.connect().await?;
    /// # let _ = &mut event_stream;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(&mut self) -> DXLinkResult<Receiver<MarketEvent>> {
        // A new session starts clean: a reason from the previous one would make
        // a healthy connection look dead.
        *recover(&self.disconnect_reason) = None;

        let (connection, negotiated) =
            handshake(&self.url, &self.token, self.keepalive_timeout).await?;
        self.server_keepalive_timeout = negotiated;

        *recover(&self.connection) = Some(connection);

        let receiver = self.event_stream();

        // Before any task: the reader is handed the session-lost sender as it
        // is spawned, so the channel has to exist first.
        if self.reconnect.is_some() && self.session_lost_sender.is_none() {
            let (lost_tx, lost_rx) = mpsc::channel::<String>(1);
            self.session_lost_sender = Some(lost_tx);
            self.session_lost_receiver = Some(lost_rx);
            let (state_tx, _) = broadcast::channel::<ConnectionState>(CONNECTION_STATE_CAPACITY);
            self.state_sender = Some(state_tx);
        }

        // Keepalive first: it owns the shutdown channel the reader needs in
        // order to stop it when the session dies. Nothing goes out in between,
        // because the first maintenance tick is a full interval away.
        self.start_keepalive()?;

        // Then the reader, which must be up before any traffic can arrive.
        self.start_message_processing()?;

        // Last, and only if the consumer asked for it. Without a policy nothing
        // is spawned and the behaviour is exactly what it was: the session dies
        // and stays dead.
        self.start_reconnect_supervisor();

        receiver
    }

    /// Starts the keepalive task.
    ///
    /// This function spawns a new tokio task that periodically sends keepalive
    /// messages to the DXLink server. The interval is derived from the deadline
    /// the server negotiated in its `SETUP` reply, not from a fixed constant, so
    /// a server asking for less than the client would otherwise assume is
    /// honoured. Maintenance is skipped when ordinary traffic has already reset
    /// the server's idle timer.
    ///
    /// The keepalive task runs in an infinite loop until either the connection is
    /// dropped or a shutdown signal is received through the `keepalive_sender` channel.
    ///
    /// # Errors
    ///
    /// Returns an error if no connection is established or if sending a keepalive
    /// message fails.
    ///
    fn start_keepalive(&mut self) -> DXLinkResult<()> {
        // Asegurarnos de que tenemos una conexión
        let connection = self.get_connection().map_err(|_| {
            DXLinkError::Connection("Cannot start keepalive without a connection".to_string())
        })?;

        // Derived from what the server negotiated, not from a fixed constant.
        let keepalive_interval = self.keepalive_interval();
        debug!(
            "Keepalive every {:?} (server asked for {:?}s)",
            keepalive_interval, self.server_keepalive_timeout
        );

        let (handle, stop) = spawn_keepalive(connection, keepalive_interval);
        // Parked in the slots, not in a field: disconnect has to be able to
        // stop whichever session is live, and a reconnect replaces these.
        *recover(&self.session_tasks.keepalive) = Some(handle);
        *recover(&self.session_tasks.keepalive_stop) = Some(stop);

        Ok(())
    }

    fn start_message_processing(&mut self) -> DXLinkResult<()> {
        // Check first: nothing may be spawned on a client that is not connected.
        let connection = self.get_connection().map_err(|_| {
            DXLinkError::Connection(
                "Cannot start message processing without a connection".to_string(),
            )
        })?;

        let delivery_tx = self.start_event_delivery();
        let reader = spawn_reader(ReaderSetup {
            connection,
            // Cloned so the reader can tear the whole session down, not just
            // itself: stopping only the reader left the keepalive writing to a
            // socket nobody was listening to.
            shutdown_keepalive: self.session_tasks.keepalive_stop(),
            delivery_tx,
            disconnect_reason: self.disconnect_reason.clone(),
            // The reader only routes protocol traffic now; callbacks and the
            // consumer stream belong to the delivery worker.
            response_requests: self.response_requests.clone(),
            channel_schemas: self.channel_schemas.clone(),
            // Inbound silence past our advertised deadline means the peer is
            // gone, even though the socket is still open. Without this the task
            // waits on a dead connection forever.
            receive_deadline: Duration::from_secs(u64::from(self.keepalive_timeout)),
            // Only when a supervisor is going to listen. A sender with no
            // receiver would make the reader do work for nobody.
            session_lost: self.session_lost_sender.clone(),
        });
        *recover(&self.session_tasks.reader) = Some(reader);

        Ok(())
    }

    /// Spawns the task that rebuilds the session after a terminal failure.
    ///
    /// Does nothing without a policy, which is what keeps "no automatic
    /// reconnection" the default.
    fn start_reconnect_supervisor(&mut self) {
        // Taken first, and unconditionally. Holding this clone past here would
        // keep the delivery worker alive on a dead session, and the closing
        // stream is how a consumer without a policy learns the session is over.
        let delivery_tx = self.pending_delivery_tx.take();

        let Some(policy) = self.reconnect.clone() else {
            return;
        };
        let Some(session_lost) = self.session_lost_sender.clone() else {
            return;
        };
        let Some(mut lost_rx) = self.session_lost_receiver.take() else {
            return;
        };
        let Some(delivery_tx) = delivery_tx else {
            return;
        };

        let ctx = ReconnectContext {
            url: self.url.clone(),
            token: self.token.clone(),
            keepalive_timeout: self.keepalive_timeout,
            connection: self.connection.clone(),
            channels: self.channels.clone(),
            channel_contracts: self.channel_contracts.clone(),
            channel_schemas: self.channel_schemas.clone(),
            subscriptions: self.subscriptions.clone(),
            response_requests: self.response_requests.clone(),
            next_request_id: self.next_request_id.clone(),
            disconnect_reason: self.disconnect_reason.clone(),
            delivery_tx,
            session_tasks: self.session_tasks.clone(),
        };

        // Cloned, not taken: a consumer can subscribe at any time, so the client
        // keeps its own sender. `disconnect` drops that one, and the supervisor
        // returning drops this one, which together close the stream.
        let states = self.state_sender.clone();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.supervisor_shutdown = Some(shutdown_tx);

        // Seeded off the wall clock so two clients started together do not pick
        // the same jitter. Only ever used to spread a delay, never for
        // anything that has to be unpredictable.
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.subsec_nanos() as u64)
            .unwrap_or(0x2545_F491_4F6C_DD1D)
            | 1;

        let handle = tokio::spawn(async move {
            loop {
                let reason = tokio::select! {
                    _ = shutdown_rx.recv() => {
                        debug!("Reconnect supervisor asked to stop");
                        return;
                    }
                    lost = lost_rx.recv() => match lost {
                        Some(reason) => reason,
                        // Every reader is gone and no new one will report, so
                        // there is nothing left to supervise.
                        None => return,
                    },
                };

                notify(
                    &states,
                    ConnectionState::Lost {
                        reason: reason.clone(),
                    },
                );

                // Waiters registered against the dead session would otherwise
                // sit until their own timeouts. Dropping the senders wakes them
                // now, with an error rather than a stall.
                recover(&ctx.response_requests).clear();

                let mut attempt: u32 = 0;
                loop {
                    attempt += 1;
                    if let Some(limit) = policy.max_attempts
                        && attempt > limit
                    {
                        let reason =
                            format!("gave up reconnecting after {limit} attempt(s): {reason}");
                        error!("{reason}");
                        notify(&states, ConnectionState::GaveUp { reason });
                        return;
                    }

                    let delay = policy.delay_for(attempt, &mut seed);
                    notify(&states, ConnectionState::Reconnecting { attempt, delay });

                    // The sleep is part of what disconnect() has to be able to
                    // cut short: waiting out a 30 second backoff before
                    // noticing would make shutdown feel hung.
                    tokio::select! {
                        _ = shutdown_rx.recv() => {
                            debug!("Reconnect supervisor stopped during backoff");
                            return;
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }

                    match reconnect_once(&ctx, session_lost.clone()).await {
                        Ok(()) => {
                            info!("Reconnected after {attempt} attempt(s)");
                            // The new session is live, so the recorded reason
                            // describes a connection that no longer applies.
                            *recover(&ctx.disconnect_reason) = None;
                            notify(&states, ConnectionState::Reconnected);
                            break;
                        }
                        Err(e) if !worth_retrying(&e) => {
                            // Documented classification: a rejected token or a
                            // protocol disagreement will fail identically next
                            // time, so retrying is just noise.
                            let reason = format!("not retrying: {e}");
                            error!("{reason}");
                            notify(&states, ConnectionState::GaveUp { reason });
                            return;
                        }
                        Err(e) => {
                            warn!("Reconnect attempt {attempt} failed: {e}");
                        }
                    }
                }
            }
        });

        self.supervisor_handle = Some(handle);
    }

    /// Close the connection and clean up resources
    pub async fn disconnect(&mut self) -> DXLinkResult<()> {
        // Order matters and it used to be backwards. The reader is the only
        // response router, so aborting it first meant every close_channel below
        // waited out its full five second timeout for a CHANNEL_CLOSED nobody
        // could deliver: five seconds per open channel, and the channel state
        // left behind anyway.
        //
        // 0. Stop the supervisor before anything else. It exists to rebuild a
        //    session that dies, and every step below looks exactly like a
        //    session dying: stopping it later would race a reconnect against
        //    the teardown.
        if let Some(stop) = self.supervisor_shutdown.take() {
            let _ = stop.send(()).await;
        }
        if let Some(mut handle) = self.supervisor_handle.take()
            && tokio::time::timeout(SHUTDOWN_GRACE, &mut handle)
                .await
                .is_err()
        {
            warn!("Reconnect supervisor did not stop within {SHUTDOWN_GRACE:?}; aborting it");
            handle.abort();
        }
        // Dropping the sender means a reader that dies during the teardown
        // below has nobody to report to, which is what we want by then.
        self.session_lost_sender = None;
        // And the state stream ends with the session. The supervisor is stopped
        // by now, whether it returned or was aborted after the grace period, and
        // either way it no longer holds a sender — so dropping this one is what
        // tells a consumer there will be no more states.
        self.state_sender = None;

        // 1. Stop writing maintenance, so nothing races the shutdown.
        // Taken in its own block: the guard must not live into the await below,
        // which is the rule for every std::sync::Mutex in this file.
        let keepalive_stop = recover(&self.session_tasks.keepalive_stop).take();
        if let Some(sender) = keepalive_stop {
            let _ = sender.send(()).await;
        }
        let keepalive = recover(&self.session_tasks.keepalive).take();
        if let Some(mut handle) = keepalive {
            // Borrow the handle rather than moving it into the timeout: a moved
            // handle is dropped when the timeout elapses, which detaches the
            // task instead of stopping it. Connection::send has no deadline, so
            // that task could sit in an await and keep using the socket after
            // disconnect returned, with nothing left to stop it.
            if tokio::time::timeout(SHUTDOWN_GRACE, &mut handle)
                .await
                .is_err()
            {
                warn!("Keepalive task did not stop within {SHUTDOWN_GRACE:?}; aborting it");
                handle.abort();
            }
        }

        // 2. Close the channels while the reader is still alive to route the
        //    replies, under one deadline for the whole set rather than per
        //    channel.
        let channels_to_close = {
            let channels = self
                .channels
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            channels.keys().copied().collect::<Vec<_>>()
        };

        if !channels_to_close.is_empty() && self.get_connection().is_ok() {
            let closing = async {
                for channel_id in channels_to_close {
                    if let Err(e) = self.close_channel(channel_id).await {
                        warn!("Error closing channel {}: {}", channel_id, e);
                    }
                }
            };
            if tokio::time::timeout(SHUTDOWN_GRACE, closing).await.is_err() {
                warn!(
                    "Gave up closing channels after {:?}; shutting down anyway",
                    SHUTDOWN_GRACE
                );
            }
        }

        // 3. Now the reader has nothing left to route.
        if let Some(handle) = recover(&self.session_tasks.reader).take() {
            handle.abort();
        }
        if let Some(handle) = self.delivery_handle.take() {
            handle.abort();
        }

        // 4. Drop the connection and every scrap of session state, so a second
        //    disconnect is a no-op and a later reconnect starts clean.
        *recover(&self.connection) = None;
        self.server_keepalive_timeout = None;
        self.clear_session_state();

        info!("Disconnected from DXLink server");

        Ok(())
    }

    /// Drops everything tied to one session.
    ///
    /// Channels, subscriptions and pending responses all describe a connection
    /// that no longer exists; carrying them across a disconnect made a later
    /// reconnect look like it already had channels open.
    ///
    /// This clears the per-session bookkeeping, which is what a future
    /// reconnect will need. It does not by itself make a
    /// disconnect-then-connect cycle work: `connect` hands out the event
    /// stream and that is still single-shot.
    fn clear_session_state(&mut self) {
        // Recovering from poisoning rather than skipping: state that describes a
        // dead connection has to go, and a prior panic elsewhere is no reason to
        // keep it.
        recover(&self.channels).clear();
        recover(&self.channel_schemas).clear();
        recover(&self.channel_contracts).clear();
        self.event_stream_taken = false;
        recover(&self.subscriptions).clear();
        // Dropping the senders wakes every waiter instead of leaving it to time
        // out against a connection that is already gone.
        recover(&self.response_requests).clear();
    }

    /// Create a channel for receiving market data
    pub async fn create_feed_channel(&mut self, contract: &str) -> DXLinkResult<u32> {
        let channel_id = self.next_channel_id()?;

        let mut params = HashMap::new();
        params.insert("contract".to_string(), contract.to_string());

        let channel_request = ChannelRequestMessage {
            channel: channel_id,
            message_type: "CHANNEL_REQUEST".to_string(),
            service: "FEED".to_string(),
            parameters: params,
        };

        let response = self
            .request_response(
                &channel_request,
                "create_feed_channel",
                "CHANNEL_OPENED",
                channel_id,
                Duration::from_secs(10),
            )
            .await?;

        match response {
            ResponseType::ChannelOpened(received_channel) => {
                if received_channel != channel_id {
                    return Err(DXLinkError::Channel(format!(
                        "Expected channel ID {}, got {}",
                        channel_id, received_channel
                    )));
                }

                // Agregar canal a la lista
                {
                    let mut channels = recover(&self.channels);
                    channels.insert(channel_id, "FEED".to_string());
                }
                // Kept so a reconnect reopens the channel with the contract it
                // had, rather than guessing one.
                recover(&self.channel_contracts).insert(channel_id, contract.to_string());

                info!("Feed channel {} created successfully", channel_id);
                Ok(channel_id)
            }
            ResponseType::Error(code, message) => Err(DXLinkError::Protocol(format!(
                "server returned {code}: {message}"
            ))),
            _ => Err(DXLinkError::Protocol(
                "Unexpected response type".to_string(),
            )),
        }
    }

    /// Setup a feed channel with desired configuration
    pub async fn setup_feed(
        &mut self,
        channel_id: u32,
        event_types: &[EventType],
    ) -> DXLinkResult<()> {
        // Validate channel exists and is a FEED channel
        self.validate_channel(channel_id, "FEED")?;

        if event_types.is_empty() {
            return Err(DXLinkError::Protocol(format!(
                "setup_feed on channel {channel_id} needs at least one event type; an empty \
                 configuration subscribes to nothing and can never deliver"
            )));
        }

        // Create event fields
        let mut accept_event_fields = HashMap::new();

        for event_type in event_types {
            // Refuse rather than half-configure. The wildcard this replaces
            // requested only eventType and eventSymbol for anything without a
            // decoder, so the server accepted the setup, accepted the
            // subscription, and then nothing ever arrived: an empty stream that
            // looks exactly like a quiet market.
            let Some(fields) = event_type.compact_fields() else {
                return Err(DXLinkError::Protocol(format!(
                    "this client cannot decode {event_type} events, so configuring channel \
                     {channel_id} for them would deliver nothing. Decoded types: {}",
                    decodable_type_names()
                )));
            };
            let fields: Vec<String> = fields.iter().map(|f| (*f).to_string()).collect();

            accept_event_fields.insert(event_type.to_string(), fields);
        }

        let feed_setup = FeedSetupMessage {
            channel: channel_id,
            message_type: "FEED_SETUP".to_string(),
            accept_aggregation_period: 0.1,
            accept_data_format: "COMPACT".to_string(),
            accept_event_fields,
        };

        // Drop any layout this channel already had, before the request goes
        // out. A reconfiguration that then fails validation used to return
        // early leaving the old entry installed, and because the reply was
        // routed to the waiter the unsolicited-config path never saw it: the
        // channel kept decoding against a contract the server had changed.
        recover(&self.channel_schemas).remove(&channel_id);

        // No direct payload log here: Connection::send already logs it through
        // the redacting path, and a second hand-rolled log is exactly how a
        // credential ends up in a file.

        let response = self
            .request_response(
                &feed_setup,
                "setup_feed",
                "FEED_CONFIG",
                channel_id,
                Duration::from_secs(10),
            )
            .await?;

        // Procesar la respuesta
        match response {
            ResponseType::FeedConfig(config) => {
                if config.channel != channel_id {
                    return Err(DXLinkError::Channel(format!(
                        "Expected config for channel {}, got {}",
                        channel_id, config.channel
                    )));
                }

                // The reply is the negotiated contract, not an
                // acknowledgement, and it is what the decoder will read against
                // — the server may serve fewer fields than were asked for.
                // Accepting it unread meant a different data format went
                // unnoticed until values landed in the wrong fields.
                let schema = negotiated_schema(&config, channel_id, event_types)?;

                recover(&self.channel_schemas).insert(channel_id, schema);

                info!("Feed channel {} setup completed successfully", channel_id);
                Ok(())
            }
            ResponseType::Error(code, message) => Err(DXLinkError::Protocol(format!(
                "server returned {code}: {message}"
            ))),
            _ => Err(DXLinkError::Protocol(
                "Unexpected response type".to_string(),
            )),
        }
    }

    /// Subscribe to market events for specific symbols
    pub async fn subscribe(
        &mut self,
        channel_id: u32,
        subscriptions: Vec<FeedSubscription>,
    ) -> DXLinkResult<()> {
        // Validate channel exists and is a FEED channel
        self.validate_channel(channel_id, "FEED")?;

        // Reject before sending, not after. A misspelled type used to go out on
        // the wire verbatim while being recorded locally as Quote, so the client
        // believed in a subscription it had never made.
        let configured = recover(&self.channel_schemas).get(&channel_id).cloned();
        let configured = configured.ok_or_else(|| {
            DXLinkError::Channel(format!(
                "channel {channel_id} has no validated feed configuration; call setup_feed first"
            ))
        })?;
        let parsed = parse_subscription_types(&subscriptions, channel_id, &configured)?;

        let subscription_msg = FeedSubscriptionMessage {
            channel: channel_id,
            message_type: "FEED_SUBSCRIPTION".to_string(),
            add: Some(subscriptions),
            remove: None,
            reset: None,
        };

        // Copy the subscriptions back before the message is moved into the
        // send: the tracked state carries fromTime and source, not just the
        // symbol, because a replay that dropped them would resubscribe live
        // where the consumer had asked for history.
        let sent: Vec<FeedSubscription> = subscription_msg.add.clone().unwrap_or_default();

        let conn = self.get_connection_mut()?;
        conn.send(&subscription_msg).await?;

        // Only now. Recording before the send meant a failed send left the
        // client believing in a subscription the server never received, which
        // is the same divergence this method was fixed to avoid.
        {
            let mut subs = recover(&self.subscriptions);
            for (event_type, subscription) in parsed.iter().zip(sent) {
                subs.insert(channel_id, *event_type, subscription);
            }
        }

        info!("Subscriptions added to channel {}", channel_id);

        Ok(())
    }

    /// Unsubscribe from market events for specific symbols
    pub async fn unsubscribe(
        &mut self,
        channel_id: u32,
        subscriptions: Vec<FeedSubscription>,
    ) -> DXLinkResult<()> {
        // Validate channel exists and is a FEED channel
        self.validate_channel(channel_id, "FEED")?;

        // Update internal subscriptions tracking
        let configured = recover(&self.channel_schemas).get(&channel_id).cloned();
        let configured = configured.ok_or_else(|| {
            DXLinkError::Channel(format!(
                "channel {channel_id} has no validated feed configuration; call setup_feed first"
            ))
        })?;
        let parsed = parse_subscription_types(&subscriptions, channel_id, &configured)?;

        let subscription_msg = FeedSubscriptionMessage {
            channel: channel_id,
            message_type: "FEED_SUBSCRIPTION".to_string(),
            add: None,
            remove: Some(subscriptions),
            reset: None,
        };

        let conn = self.get_connection_mut()?;
        conn.send(&subscription_msg).await?;

        // After the send, for the same reason as subscribe: a failed send must
        // not leave the client believing it unsubscribed. Matched on the whole
        // identity, source included, so removing one source's subscription does
        // not forget another's for the same symbol.
        if let Some(removed) = subscription_msg.remove.as_ref() {
            let mut subs = recover(&self.subscriptions);
            for (event_type, subscription) in parsed.iter().zip(removed) {
                subs.remove(channel_id, *event_type, subscription);
            }
        }

        info!("Subscriptions removed from channel {}", channel_id);

        Ok(())
    }

    /// Resets the subscriptions on one channel, leaving the subscriptions on
    /// every other channel untouched.
    ///
    /// The reset the server performs is scoped to the channel it arrives on, so
    /// the tracked state now matches: this used to clear every channel's
    /// subscriptions, which left the client blind to symbols that were still
    /// being delivered.
    pub async fn reset_subscriptions(&mut self, channel_id: u32) -> DXLinkResult<()> {
        // Validate channel exists and is a FEED channel
        self.validate_channel(channel_id, "FEED")?;

        let subscription_msg = FeedSubscriptionMessage {
            channel: channel_id,
            message_type: "FEED_SUBSCRIPTION".to_string(),
            add: None,
            remove: None,
            reset: Some(true),
        };

        let conn = self.get_connection_mut()?;
        conn.send(&subscription_msg).await?;

        // After the send, like subscribe and unsubscribe: a reset that never
        // left the client must not be recorded as one that did.
        recover(&self.subscriptions).forget_channel(channel_id);

        info!("All subscriptions reset on channel {}", channel_id);

        Ok(())
    }

    /// Close a channel
    pub async fn close_channel(&mut self, channel_id: u32) -> DXLinkResult<()> {
        // Check if the channel exists
        {
            let channels = recover(&self.channels);
            if !channels.contains_key(&channel_id) {
                return Err(DXLinkError::Channel(format!(
                    "Channel {} not found",
                    channel_id
                )));
            }
        }

        // Crear el mensaje de cancelación
        let cancel_msg = BaseMessage {
            channel: channel_id,
            message_type: "CHANNEL_CANCEL".to_string(),
        };

        let response = self
            .request_response(
                &cancel_msg,
                "close_channel",
                "CHANNEL_CLOSED",
                channel_id,
                Duration::from_secs(5),
            )
            .await?;

        // Procesar la respuesta
        match response {
            ResponseType::ChannelClosed(received_channel) => {
                if received_channel != channel_id {
                    return Err(DXLinkError::Channel(format!(
                        "Expected CHANNEL_CLOSED for channel {}, got {}",
                        channel_id, received_channel
                    )));
                }

                // Remove channel from list
                {
                    let mut channels = recover(&self.channels);
                    channels.remove(&channel_id);
                }
                // A closed channel delivers nothing, so its subscriptions and
                // its validated layouts are gone with it. Leaving them behind
                // made a later replay resubscribe on a channel that no longer
                // existed.
                recover(&self.subscriptions).forget_channel(channel_id);
                recover(&self.channel_schemas).remove(&channel_id);
                recover(&self.channel_contracts).remove(&channel_id);

                info!("Channel {} closed successfully", channel_id);
                Ok(())
            }
            ResponseType::Error(code, message) => Err(DXLinkError::Protocol(format!(
                "server returned {code}: {message}"
            ))),
            _ => Err(DXLinkError::Protocol(
                "Unexpected response type".to_string(),
            )),
        }
    }

    /// The subscriptions this client believes are live on a channel, in the
    /// order they were asked for.
    ///
    /// Only what the server acknowledged by accepting the send: a subscription
    /// whose `FEED_SUBSCRIPTION` failed to go out never appears here. An unknown
    /// or closed channel gives an empty list rather than an error, because
    /// "nothing is subscribed there" is the true answer in both cases.
    ///
    /// Additive: a new method, no existing signature changes.
    pub fn subscriptions(&self, channel_id: u32) -> Vec<FeedSubscription> {
        recover(&self.subscriptions).of_channel(channel_id)
    }

    /// The channels that currently hold at least one subscription, lowest id
    /// first.
    ///
    /// Additive: a new method, no existing signature changes.
    pub fn subscribed_channels(&self) -> Vec<u32> {
        recover(&self.subscriptions).channels()
    }

    /// Register a callback function for a specific symbol
    pub fn on_event(&self, symbol: &str, callback: impl Fn(MarketEvent) + Send + Sync + 'static) {
        let mut callbacks = recover(&self.callbacks);
        callbacks.insert(
            symbol.to_string(),
            Arc::new(Box::new(callback) as EventCallback),
        );
    }

    /// Get a stream of market events.
    ///
    /// There is exactly one per client, and `connect` already returns it. If the
    /// receiver is dropped the stream cannot be re-taken: delivery continues to
    /// registered callbacks only, and the loss is logged once rather than per
    /// event.
    ///
    /// # Backpressure
    ///
    /// The stream is bounded and **events are dropped when the consumer falls
    /// behind**, rather than the library blocking to preserve them. A blocked
    /// consumer would otherwise stall the socket reader and make unrelated
    /// channel operations time out, and a quote that arrives late is worth less
    /// than the connection staying responsive. Drops are logged. If you cannot
    /// afford to miss events, drain this receiver into your own unbounded
    /// buffer as soon as it yields.
    pub fn event_stream(&mut self) -> DXLinkResult<Receiver<MarketEvent>> {
        if self.event_stream_taken {
            return Err(DXLinkError::Protocol(
                "Event stream already created".to_string(),
            ));
        }
        let (tx, rx) = mpsc::channel(100); // Buffer of 100 events
        self.event_sender = Some(tx);
        self.event_stream_taken = true;
        Ok(rx)
    }

    /// Why the session ended, if it has ended.
    ///
    /// The event stream closing tells a consumer *that* the session is over;
    /// this tells it *why*. `None` while the session is healthy, and after a
    /// deliberate [`disconnect`](Self::disconnect).
    ///
    /// Returned as text rather than a [`DXLinkError`] because the error is
    /// observed once inside the reader and may be reported to any number of
    /// callers, and `DXLinkError` is not `Clone`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use dxlink::DXLinkClient;
    /// # async fn example(mut client: DXLinkClient) -> Result<(), Box<dyn std::error::Error>> {
    /// let mut events = client.connect().await?;
    /// while let Some(event) = events.recv().await {
    ///     let _ = event;
    /// }
    /// // The stream closed: the session is over.
    /// if let Some(reason) = client.disconnect_reason() {
    ///     eprintln!("connection lost: {reason}");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn disconnect_reason(&self) -> Option<String> {
        recover(&self.disconnect_reason).clone()
    }

    // Helper methods
    fn next_channel_id(&self) -> DXLinkResult<u32> {
        let mut id = recover(&self.next_channel_id);
        let channel_id = *id;
        *id += 1;
        Ok(channel_id)
    }

    fn get_connection_mut(&mut self) -> DXLinkResult<WebSocketConnection> {
        live_connection(&self.connection)
    }

    fn validate_channel(&self, channel_id: u32, expected_service: &str) -> DXLinkResult<()> {
        let channels = recover(&self.channels);
        match channels.get(&channel_id) {
            Some(service) if service == expected_service => Ok(()),
            Some(service) => Err(DXLinkError::Channel(format!(
                "Channel {} is a {} channel, not a {} channel",
                channel_id, service, expected_service
            ))),
            None => Err(DXLinkError::Channel(format!(
                "Channel {} not found",
                channel_id
            ))),
        }
    }
}

/// Aborts the tasks this client owns if it is dropped without `disconnect`.
///
/// Only abort: `Drop` is synchronous, so the protocol goodbye cannot be sent
/// from here. Leaking two tasks per forgotten client is the failure this
/// prevents, not an excuse to skip `disconnect`.
impl Drop for DXLinkClient {
    fn drop(&mut self) {
        // The supervisor first: it owns the token and the delivery sender, and
        // detaching it would leave something able to reconnect after the client
        // it belongs to is gone.
        if let Some(handle) = self.supervisor_handle.take() {
            handle.abort();
        }
        // Whichever session is live, which is not necessarily the one this
        // client spawned.
        self.session_tasks.abort();
        if let Some(handle) = self.delivery_handle.take() {
            handle.abort();
        }
    }
}

impl fmt::Debug for DXLinkClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("DXLinkClient");

        debug_struct.field("url", &self.url);
        debug_struct.field("has_token", &(!self.token.is_empty()));
        debug_struct.field("connected", &self.get_connection().is_ok());
        debug_struct.field("keepalive_timeout", &self.keepalive_timeout);
        let channel_count = if let Ok(channels) = self.channels.lock() {
            channels.len()
        } else {
            0
        };
        debug_struct.field("channel_count", &channel_count);

        let callback_count = if let Ok(callbacks) = self.callbacks.lock() {
            callbacks.len()
        } else {
            0
        };
        debug_struct.field("callback_count", &callback_count);

        let subscription_count = if let Ok(subscriptions) = self.subscriptions.lock() {
            subscriptions.len()
        } else {
            0
        };
        debug_struct.field("subscription_count", &subscription_count);
        debug_struct.field("has_event_sender", &self.event_sender.is_some());
        debug_struct.field(
            "keepalive_active",
            &recover(&self.session_tasks.keepalive).is_some(),
        );
        debug_struct.field(
            "message_handler_active",
            &recover(&self.session_tasks.reader).is_some(),
        );

        let pending_responses = if let Ok(requests) = self.response_requests.lock() {
            requests.len()
        } else {
            0
        };
        debug_struct.field("pending_responses", &pending_responses);
        debug_struct.finish()
    }
}

impl fmt::Display for DXLinkClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Start with basic connection information
        write!(
            f,
            "DXLink Client [{}]",
            if self.get_connection().is_ok() {
                "Connected"
            } else {
                "Disconnected"
            }
        )?;

        // Show server URL
        write!(f, " to {}", self.url)?;

        // Add summary of active channels and subscriptions
        let channel_count = self.channels.lock().map(|c| c.len()).unwrap_or(0);
        let subscription_count = self.subscriptions.lock().map(|s| s.len()).unwrap_or(0);

        // Display active resources
        write!(
            f,
            " | Channels: {}, Subscriptions: {}",
            channel_count, subscription_count
        )?;

        // Show active tasks status
        let tasks_status = match (
            recover(&self.session_tasks.reader).is_some(),
            recover(&self.session_tasks.keepalive).is_some(),
        ) {
            (true, true) => "All tasks running",
            (true, false) => "Message handler only",
            (false, true) => "Keepalive only",
            (false, false) => "No tasks running",
        };

        write!(f, " | {}", tasks_status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::QuoteEvent;

    // Basic test for client creation
    #[test]
    fn test_new_client() {
        let client = DXLinkClient::new("wss://test.url", "test_token");

        assert_eq!(client.url, "wss://test.url");
        assert_eq!(client.token, "test_token");
        assert_eq!(client.keepalive_timeout, DEFAULT_KEEPALIVE_TIMEOUT);
        assert!(client.get_connection().is_err());
        assert!(client.event_sender.is_none());
        assert!(recover(&client.session_tasks.keepalive).is_none());
        assert!(recover(&client.session_tasks.reader).is_none());
        assert!(recover(&client.session_tasks.keepalive_stop).is_none());
    }

    // Test next_channel_id
    #[test]
    fn test_next_channel_id() {
        let client = DXLinkClient::new("wss://test.url", "test_token");

        // Get the first channel ID
        let id1 = client.next_channel_id().unwrap();

        // Get the second channel ID
        let id2 = client.next_channel_id().unwrap();

        // Check that IDs are incrementing
        assert_eq!(id2, id1 + 1);
    }

    // Test validate_channel
    #[test]
    fn test_validate_channel() {
        let client = DXLinkClient::new("wss://test.url", "test_token");

        // Add some channels
        {
            let mut channels = client.channels.lock().unwrap();
            channels.insert(1, "FEED".to_string());
            channels.insert(2, "OTHER".to_string());
        }

        // Test validating an existing channel with correct service
        let result = client.validate_channel(1, "FEED");
        assert!(result.is_ok());

        // Test validating an existing channel with wrong service
        let result = client.validate_channel(1, "OTHER");
        assert!(result.is_err());
        match result {
            Err(DXLinkError::Channel(_)) => {}
            _ => panic!("Expected Channel error"),
        }

        // Test validating a non-existent channel
        let result = client.validate_channel(3, "FEED");
        assert!(result.is_err());
        match result {
            Err(DXLinkError::Channel(_)) => {}
            _ => panic!("Expected Channel error"),
        }
    }

    // Test on_event
    #[test]
    fn test_on_event() {
        let client = DXLinkClient::new("wss://test.url", "test_token");

        // Use a flag to check if callback was called
        let called = Arc::new(Mutex::new(false));
        let called_clone = called.clone();

        // Register a callback
        client.on_event("AAPL", move |_| {
            let mut called = called_clone.lock().unwrap();
            *called = true;
        });

        // Check that callback was registered
        let callbacks = client.callbacks.lock().unwrap();
        assert!(callbacks.contains_key("AAPL"));

        // Test the callback
        if let Some(callback) = callbacks.get("AAPL") {
            let quote_event = QuoteEvent {
                event_type: "Quote".to_string(),
                event_symbol: "AAPL".to_string(),
                bid_price: 150.25,
                ask_price: 150.50,
                bid_size: 100.0,
                ask_size: 150.0,
            };

            callback(MarketEvent::Quote(quote_event));

            // Check that callback was called
            let called = called.lock().unwrap();
            assert!(*called);
        } else {
            panic!("Callback was not registered");
        }
    }

    // Test event_stream
    #[test]
    fn test_event_stream() {
        let mut client = DXLinkClient::new("wss://test.url", "test_token");

        // Check that we can get an event stream
        let result = client.event_stream();
        assert!(result.is_ok());

        // Check that we can't get a second event stream
        let result = client.event_stream();
        assert!(result.is_err());
        match result {
            Err(DXLinkError::Protocol(msg)) => {
                assert!(msg.contains("Event stream already created"));
            }
            _ => panic!("Expected Protocol error"),
        }
    }

    /// The message task must stop once the connection is gone, instead of
    /// re-reading a dead socket every 100ms forever (issue #4).
    #[tokio::test]
    async fn test_message_task_stops_when_server_closes() {
        use tokio::net::TcpListener;
        use tokio_tungstenite::accept_async;

        // A bare WebSocket server that accepts the handshake and immediately
        // hangs up. No DXLink handshake is needed: start_message_processing only
        // requires a live connection.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("failed to bind test server");
        let addr = listener.local_addr().expect("failed to read local addr");

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("failed to accept");
            let ws = accept_async(stream).await.expect("failed to handshake");
            drop(ws);
        });

        let mut client = DXLinkClient::new(&format!("ws://{}", addr), "test_token");
        *recover(&client.connection) = Some(
            crate::connection::WebSocketConnection::connect(&format!("ws://{}", addr))
                .await
                .expect("failed to connect"),
        );

        client
            .start_message_processing()
            .expect("failed to start message processing");

        let handle = recover(&client.session_tasks.reader)
            .take()
            .expect("message task should have been spawned");

        // Awaiting the handle rather than polling is_finished() also fails the
        // test if the task panicked on its way out. Generous bound: the point is
        // that it terminates at all, not how fast.
        let joined = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("message task kept running after the server closed the connection");

        joined.expect("message task panicked instead of stopping cleanly");
    }

    /// A dead session must stop both owned tasks. Stopping only the reader left
    /// the keepalive writing to a socket nobody was listening to, which is the
    /// dead-but-open state this was meant to end.
    #[tokio::test]
    async fn test_session_death_stops_both_tasks() {
        use tokio::net::TcpListener;
        use tokio_tungstenite::accept_async;

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("failed to bind test server");
        let addr = listener.local_addr().expect("failed to read local addr");

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("failed to accept");
            let ws = accept_async(stream).await.expect("failed to handshake");
            drop(ws);
        });

        let mut client = DXLinkClient::new(&format!("ws://{}", addr), "test_token");
        *recover(&client.connection) = Some(
            crate::connection::WebSocketConnection::connect(&format!("ws://{}", addr))
                .await
                .expect("failed to connect"),
        );

        client.start_keepalive().expect("failed to start keepalive");
        client
            .start_message_processing()
            .expect("failed to start message processing");

        let message = recover(&client.session_tasks.reader)
            .take()
            .expect("no message task");
        let keepalive = recover(&client.session_tasks.keepalive)
            .take()
            .expect("no keepalive task");

        tokio::time::timeout(Duration::from_secs(5), message)
            .await
            .expect("the reader kept running after the server closed")
            .expect("the reader panicked");

        // The reader is responsible for telling the writer to stop as well.
        tokio::time::timeout(Duration::from_secs(5), keepalive)
            .await
            .expect("the keepalive kept writing to a dead socket")
            .expect("the keepalive panicked");
    }

    /// A panic while holding client state must not turn every later request
    /// into a second panic. The state behind these locks is bookkeeping, not an
    /// invariant a partial write could corrupt.
    #[test]
    fn test_a_poisoned_lock_does_not_take_the_client_down() {
        let client = DXLinkClient::new("wss://example.com", "token");

        // Poison the channels lock the way a panicking thread would.
        let channels = client.channels.clone();
        let _ = std::thread::spawn(move || {
            let _guard = channels.lock().unwrap();
            panic!("poisoning the lock");
        })
        .join();
        assert!(
            client.channels.lock().is_err(),
            "the lock should be poisoned"
        );

        // Every path over that state must still work.
        {
            let mut channels = recover(&client.channels);
            channels.insert(1, "FEED".to_string());
        }
        assert!(client.validate_channel(1, "FEED").is_ok());
        assert!(client.next_channel_id().is_ok());

        client.on_event("AAPL", |_| {});
        assert!(recover(&client.callbacks).contains_key("AAPL"));
    }

    // Test error cases for connection
    #[test]
    fn test_connection_errors() {
        let mut client = DXLinkClient::new("wss://test.url", "test_token");

        // Test starting keepalive without connection
        let result = client.start_keepalive();
        assert!(result.is_err());
        match result {
            Err(DXLinkError::Connection(_)) => {}
            _ => panic!("Expected Connection error"),
        }

        // Test starting message processing without connection
        let result = client.start_message_processing();
        assert!(result.is_err());
        match result {
            Err(DXLinkError::Connection(_)) => {}
            _ => panic!("Expected Connection error"),
        }

        // Test getting connection without having one
        let result = client.get_connection_mut();
        assert!(result.is_err());
        match result {
            Err(DXLinkError::Connection(_)) => {}
            _ => panic!("Expected Connection error"),
        }
    }
}

#[cfg(test)]
mod version_tests {
    use super::DEFAULT_CLIENT_VERSION;

    /// The exact string the server sees. Pinned because a malformed version is
    /// invisible in normal operation and only shows up in someone else's
    /// telemetry.
    #[test]
    fn test_setup_version_follows_the_spec_format() {
        let (protocol_and_impl, client_version) = DEFAULT_CLIENT_VERSION
            .split_once('/')
            .expect("version must be <protocol>-<implementation>/<client-version>");

        let (protocol, implementation) = protocol_and_impl
            .split_once('-')
            .expect("the part before / must be <protocol>-<implementation>");

        assert_eq!(protocol, "0.1", "protocol version token");
        assert_eq!(implementation, "dxlink-rs", "implementation token");

        // Bumping Cargo.toml must move this without another edit.
        assert_eq!(
            client_version,
            env!("CARGO_PKG_VERSION"),
            "the advertised version must be the crate version"
        );
        assert!(
            !client_version.is_empty() && client_version.contains('.'),
            "prerelease and normal versions alike are carried verbatim: {client_version}"
        );
    }
}

#[cfg(test)]
mod keepalive_tests {
    use super::*;

    #[test]
    fn test_interval_follows_the_negotiated_deadline() {
        let mut client = DXLinkClient::new("wss://example.com", "token");

        // Nothing negotiated yet: fall back to what we advertise.
        assert_eq!(
            client.keepalive_interval(),
            Duration::from_secs(
                u64::from(DEFAULT_KEEPALIVE_TIMEOUT).div_ceil(u64::from(KEEPALIVE_DIVISOR))
            )
        );

        // A server asking for less than the old fixed 15s must be honoured, or
        // it closes the connection while the client thinks it is healthy.
        client.server_keepalive_timeout = Some(3);
        assert_eq!(client.keepalive_interval(), Duration::from_secs(1));

        client.server_keepalive_timeout = Some(60);
        assert_eq!(
            client.keepalive_interval(),
            Duration::from_secs(60 / u64::from(KEEPALIVE_DIVISOR))
        );
    }

    #[test]
    fn test_interval_never_collapses_to_zero() {
        let mut client = DXLinkClient::new("wss://example.com", "token");
        // Would round down to zero and spin the task.
        client.server_keepalive_timeout = Some(1);
        assert!(client.keepalive_interval() >= MIN_KEEPALIVE_INTERVAL);
    }

    #[test]
    fn test_advertised_timeout_is_configurable_and_validated() {
        let client = DXLinkClient::new("wss://example.com", "token")
            .with_keepalive_timeout(30)
            .expect("30 is valid");
        assert_eq!(client.keepalive_timeout, 30);

        let rejected = DXLinkClient::new("wss://example.com", "token").with_keepalive_timeout(0);
        assert!(matches!(rejected, Err(DXLinkError::Protocol(_))));
    }
}

#[cfg(test)]
mod reconnect_policy_tests {
    use super::*;

    fn policy() -> ReconnectPolicy {
        ReconnectPolicy {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
            max_attempts: None,
            jitter: false,
        }
    }

    #[test]
    fn test_the_delay_doubles_and_then_stops_at_the_ceiling() {
        let policy = policy();
        let mut seed = 1;
        let delays: Vec<u64> = (1..=6)
            .map(|attempt| policy.delay_for(attempt, &mut seed).as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 8, 8]);
    }

    /// A long outage must not fold the backoff back round to zero and start
    /// hammering, which is what an overflowing shift would do.
    #[test]
    fn test_a_very_late_attempt_still_waits_the_maximum() {
        let policy = policy();
        let mut seed = 1;
        for attempt in [40u32, 1_000, u32::MAX] {
            assert_eq!(
                policy.delay_for(attempt, &mut seed),
                policy.max_delay,
                "attempt {attempt} escaped the ceiling"
            );
        }
    }

    #[test]
    fn test_jitter_stays_inside_the_backoff_and_moves() {
        let policy = ReconnectPolicy {
            jitter: true,
            ..policy()
        };
        let mut seed = 0x2545_F491_4F6C_DD1D;
        let mut seen = Vec::new();
        for _ in 0..16 {
            let delay = policy.delay_for(4, &mut seed);
            assert!(
                delay <= policy.max_delay,
                "jitter must never exceed the delay it spreads: {delay:?}"
            );
            seen.push(delay);
        }
        // The point of jitter is that two clients do not come back together.
        assert!(
            seen.iter().any(|delay| *delay != seen[0]),
            "jitter produced the same delay every time: {seen:?}"
        );
    }

    /// A policy asking for nothing must still pace itself: zero delays with
    /// unlimited attempts is a request and log flood, not a reconnect.
    #[test]
    fn test_a_zero_delay_is_clamped_to_a_floor() {
        let zero = ReconnectPolicy {
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            max_attempts: None,
            jitter: false,
        };
        let mut seed = 1;
        for attempt in 1..=5 {
            assert_eq!(zero.delay_for(attempt, &mut seed), MIN_RECONNECT_DELAY);
        }

        // And with jitter, which spreads over [0, delay] and could otherwise
        // put it straight back to zero.
        let jittered = ReconnectPolicy {
            jitter: true,
            ..zero
        };
        let mut seed = 0x2545_F491_4F6C_DD1D;
        for attempt in 1..=32 {
            assert!(jittered.delay_for(attempt, &mut seed) >= MIN_RECONNECT_DELAY);
        }
    }

    #[test]
    fn test_only_transport_failures_are_worth_retrying() {
        // What reconnection exists for.
        assert!(worth_retrying(&DXLinkError::Connection("gone".into())));
        assert!(worth_retrying(&DXLinkError::Channel(
            "no such channel".into()
        )));
        // The case is_terminal gets wrong for this question: a peer that
        // accepts the socket and never answers has to be retried.
        assert!(worth_retrying(&DXLinkError::Timeout(
            "no FEED_CONFIG".into()
        )));

        // Fails identically next time.
        assert!(!worth_retrying(&DXLinkError::Authentication(
            "bad token".into()
        )));
        assert!(!worth_retrying(&DXLinkError::Protocol(
            "wrong layout".into()
        )));
        assert!(!worth_retrying(&DXLinkError::UnexpectedMessage("?".into())));
        assert!(!worth_retrying(&DXLinkError::Unknown("?".into())));
    }

    #[test]
    fn test_reconnection_is_off_until_it_is_asked_for() {
        let mut client = DXLinkClient::new("wss://example.com", "token");
        assert!(client.reconnect.is_none(), "no policy by default");
        assert!(
            client.connection_states().is_none(),
            "no state stream without a policy"
        );

        client.with_reconnect(ReconnectPolicy::default());
        assert!(client.reconnect.is_some());

        // Available from here, without connecting first: a `&self` accessor
        // that answered None until some other call had happened is a trap.
        assert!(
            client.connection_states().is_some(),
            "the stream should exist as soon as the policy does"
        );
        // And it is a subscription, not a handout, so a second caller gets one
        // of their own.
        assert!(client.connection_states().is_some());
    }
}
