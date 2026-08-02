/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use crate::connection::{WebSocketConnection, sanitize_server_text};
use crate::error::{DXLinkError, DXLinkResult};
use crate::events::{CompactData, EventType, MarketEvent};
use crate::messages::{
    AuthMessage, AuthStateMessage, BaseMessage, ChannelRequestMessage, ErrorMessage,
    FeedDataMessage, FeedSetupMessage, FeedSubscription, FeedSubscriptionMessage, KeepaliveMessage,
    ServerSetupMessage, SetupMessage,
};

use crate::parse_compact_data;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
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

/// Capacity of the queue between the socket reader and the delivery worker.
///
/// Generous: it only has to absorb a burst while the worker is inside a user
/// callback. When it does fill, events are dropped rather than blocking the
/// reader, because a blocked reader stops answering protocol traffic and makes
/// unrelated channel operations time out.
const DELIVERY_QUEUE_CAPACITY: usize = 1024;

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
    /// Indicates a feed configuration has been received.  The `u32` value represents the channel identifier.
    FeedConfig(u32),
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
        if let Ok(mut requests) = self.requests.lock() {
            requests.retain(|request| request.id != self.id);
        }
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
/// * `subscriptions`: A thread-safe set that keeps track of active subscriptions,
///   identified by pairs of `EventType` and the corresponding market data symbol.
///   This ensures that duplicate subscriptions are avoided and allows for efficient
///   management of subscriptions.  It uses `Arc<Mutex>` for thread safety.
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
    /// The active WebSocket connection, if established.  `None` indicates no active connection.
    connection: Option<WebSocketConnection>,
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
    /// A thread-safe set keeping track of active subscriptions, identified by `(EventType, String)`.
    subscriptions: Arc<Mutex<HashSet<(EventType, String)>>>, // (event_type, symbol)
    /// A sender for transmitting `MarketEvent` instances.
    event_sender: Option<Sender<MarketEvent>>,
    /// A handle to the keepalive task.
    keepalive_handle: Option<JoinHandle<()>>,
    /// A handle to the message processing task.
    message_handle: Option<JoinHandle<()>>,
    /// A handle to the task that runs callbacks and feeds the event stream.
    delivery_handle: Option<JoinHandle<()>>,
    /// A channel sender used to signal the keepalive task.
    keepalive_sender: Option<Sender<()>>,
    /// A thread-safe vector that holds pending response requests.
    response_requests: Arc<Mutex<Vec<ResponseRequest>>>,
    /// Hands out the identity each pending request is cleaned up by.
    next_request_id: Arc<Mutex<u64>>,
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
            connection: None,
            keepalive_timeout: DEFAULT_KEEPALIVE_TIMEOUT,
            server_keepalive_timeout: None,
            next_channel_id: Arc::new(Mutex::new(1)), // Start from 1 as 0 is the main channel
            channels: Arc::new(Mutex::new(HashMap::new())),
            callbacks: Arc::new(Mutex::new(HashMap::new())),
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
            event_sender: None,
            keepalive_handle: None,
            message_handle: None,
            delivery_handle: None,
            keepalive_sender: None,
            response_requests: Arc::new(Mutex::new(Vec::new())),
            next_request_id: Arc::new(Mutex::new(0)),
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
        let (tx, rx) = oneshot::channel();

        let id = {
            let mut next = self
                .next_request_id
                .lock()
                .map_err(|_| DXLinkError::Unknown("request id lock poisoned".to_string()))?;
            *next += 1;
            *next
        };

        {
            let mut requests = self
                .response_requests
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
            requests: Arc::clone(&self.response_requests),
            id,
        };

        self.get_connection()?.send(message).await?;

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

    /// Borrows the live connection.
    fn get_connection(&self) -> DXLinkResult<&WebSocketConnection> {
        self.connection
            .as_ref()
            .ok_or_else(|| DXLinkError::Connection("Not connected to DXLink server".to_string()))
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
        let event_sender = self.event_sender.clone();

        let handle = tokio::spawn(async move {
            let mut stream_closed_reported = false;

            while let Some(event) = delivery_rx.recv().await {
                let symbol = match &event {
                    MarketEvent::Quote(e) => e.event_symbol.clone(),
                    MarketEvent::Trade(e) => e.event_symbol.clone(),
                    MarketEvent::Greeks(e) => e.event_symbol.clone(),
                };

                // Copy the handle out and release the lock before running user
                // code: holding it across a callback serialises every other
                // symbol behind whatever that callback is doing.
                let callback = match callbacks.lock() {
                    Ok(map) => map.get(&symbol).cloned(),
                    Err(poisoned) => poisoned.into_inner().get(&symbol).cloned(),
                };

                if let Some(callback) = callback {
                    let delivered = event.clone();
                    // A panicking callback used to take the whole task with it,
                    // and with it every protocol response.
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        callback(delivered)
                    }))
                    .is_err()
                    {
                        error!("Callback for {} panicked; continuing delivery", symbol);
                    }
                }

                if let Some(tx) = &event_sender {
                    match tx.try_send(event) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            debug!("Event stream full, dropping an event for {}", symbol);
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
        delivery_tx
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
        let deadline = self
            .server_keepalive_timeout
            .unwrap_or(self.keepalive_timeout);
        Duration::from_secs(u64::from(deadline.div_ceil(KEEPALIVE_DIVISOR)))
            .max(MIN_KEEPALIVE_INTERVAL)
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
        // Connect to WebSocket
        let connection = WebSocketConnection::connect(&self.url).await?;

        // Send SETUP message
        let setup_msg = SetupMessage {
            channel: MAIN_CHANNEL,
            message_type: "SETUP".to_string(),
            keepalive_timeout: self.keepalive_timeout,
            accept_keepalive_timeout: self.keepalive_timeout,
            version: DEFAULT_CLIENT_VERSION.to_string(),
        };

        connection.send(&setup_msg).await?;

        // Every read below is bounded and validated. On any failure `connection`
        // is dropped here, so a partial handshake never leaves the client
        // holding a half-established socket.
        let response =
            expect_handshake_message(&connection, "SETUP", "SETUP", MAIN_CHANNEL).await?;
        let server_setup: ServerSetupMessage = serde_json::from_str(&response)?;
        debug!(
            "Server SETUP: version={:?}, keepaliveTimeout={:?}",
            server_setup.version, server_setup.keepalive_timeout
        );
        // A zero would schedule maintenance in a tight loop; treat it as "not
        // negotiated" rather than trusting it.
        self.server_keepalive_timeout = server_setup.keepalive_timeout.filter(|t| *t > 0);

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
                token: self.token.clone(),
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

        self.connection = Some(connection);

        let receiver = self.event_stream();

        // Keepalive first: it owns the shutdown channel the reader needs in
        // order to stop it when the session dies. Nothing goes out in between,
        // because the first maintenance tick is a full interval away.
        self.start_keepalive()?;

        // Then the reader, which must be up before any traffic can arrive.
        self.start_message_processing()?;

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
        if self.connection.is_none() {
            return Err(DXLinkError::Connection(
                "Cannot start keepalive without a connection".to_string(),
            ));
        }

        // Crear un canal para señales de cierre
        let (tx, mut rx) = mpsc::channel::<()>(1);
        self.keepalive_sender = Some(tx);

        // Obtener la conexión
        let connection = self.connection.as_ref().unwrap().clone();

        // Derived from what the server negotiated, not from a fixed constant.
        let keepalive_interval = self.keepalive_interval();
        debug!(
            "Keepalive every {:?} (server asked for {:?}s)",
            keepalive_interval, self.server_keepalive_timeout
        );

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

        self.keepalive_handle = Some(keepalive_handle);

        Ok(())
    }

    fn start_message_processing(&mut self) -> DXLinkResult<()> {
        // Check first: nothing may be spawned on a client that is not connected.
        if self.connection.is_none() {
            return Err(DXLinkError::Connection(
                "Cannot start message processing without a connection".to_string(),
            ));
        }

        let connection = self.connection.as_ref().unwrap().clone();

        // Cloned so the reader can tear the whole session down, not just itself:
        // stopping only the reader left the keepalive writing to a socket
        // nobody was listening to.
        let shutdown_keepalive = self.keepalive_sender.clone();
        let delivery_tx = self.start_event_delivery();

        // The reader only routes protocol traffic now; callbacks and the
        // consumer stream belong to the delivery worker.
        let response_requests = self.response_requests.clone();

        // Iniciar la tarea de procesamiento de mensajes
        // Inbound silence past our advertised deadline means the peer is gone,
        // even though the socket is still open. Without this the task waits on a
        // dead connection forever.
        let receive_deadline = Duration::from_secs(u64::from(self.keepalive_timeout));

        let message_handle = tokio::spawn(async move {
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
                        error!(
                            "{}",
                            DXLinkError::Timeout(format!(
                                "no message received on channel {} for {}s, the connection is assumed dead",
                                MAIN_CHANNEL,
                                receive_deadline.as_secs()
                            ))
                        );
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
                                if let Ok(mut requests) = response_requests.lock() {
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
                                                ResponseType::FeedConfig(channel.unwrap_or(0))
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
                                "FEED_DATA" => {
                                    if let Ok(data_msg) = serde_json::from_str::<
                                        FeedDataMessage<Vec<CompactData>>,
                                    >(&msg)
                                    {
                                        // Hand off and keep reading. Delivery is
                                        // somebody else's job: doing it here let
                                        // one slow callback or a full consumer
                                        // stream stop the socket reads that every
                                        // channel operation depends on.
                                        for event in parse_compact_data(&data_msg.data) {
                                            if delivery_tx.try_send(event).is_err() {
                                                dropped_events += 1;
                                                if dropped_events.is_power_of_two() {
                                                    warn!(
                                                        "Delivery queue full, {} event(s) dropped so far; \
                                                         the consumer is slower than the feed",
                                                        dropped_events
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                "ERROR" => {
                                    if let Ok(error_msg) =
                                        serde_json::from_str::<ErrorMessage>(&msg)
                                    {
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
        });

        self.message_handle = Some(message_handle);
        Ok(())
    }

    /// Close the connection and clean up resources
    pub async fn disconnect(&mut self) -> DXLinkResult<()> {
        // Señalizar a la tarea de keepalive que termine
        if let Some(sender) = &self.keepalive_sender {
            // Intentar enviar la señal, pero no bloquear si el receptor ya no existe
            let _ = sender.send(()).await;
            self.keepalive_sender = None;
        }

        // Esperar a que la tarea de keepalive termine
        if let Some(handle) = self.keepalive_handle.take() {
            handle.abort();
        }

        // Terminar la tarea de procesamiento de mensajes
        if let Some(handle) = self.message_handle.take() {
            handle.abort();
        }

        // The delivery worker ends on its own once the reader drops the queue,
        // but disconnect must not leave it running if the reader was aborted
        // mid-flight.
        if let Some(handle) = self.delivery_handle.take() {
            handle.abort();
        }

        // Cerrar todos los canales
        let channels_to_close = {
            let channels = self.channels.lock().unwrap();
            channels.keys().cloned().collect::<Vec<_>>()
        };

        for channel_id in channels_to_close {
            if let Err(e) = self.close_channel(channel_id).await {
                warn!("Error closing channel {}: {}", channel_id, e);
                // Continue with other channels
            }
        }

        // Cerrar la conexión
        self.connection = None;

        info!("Disconnected from DXLink server");

        Ok(())
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
                    let mut channels = self.channels.lock().unwrap();
                    channels.insert(channel_id, "FEED".to_string());
                }

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

        // Create event fields
        let mut accept_event_fields = HashMap::new();

        for event_type in event_types {
            let fields = match event_type {
                EventType::Quote => vec![
                    "eventType".to_string(),
                    "eventSymbol".to_string(),
                    "bidPrice".to_string(),
                    "askPrice".to_string(),
                    "bidSize".to_string(),
                    "askSize".to_string(),
                ],
                EventType::Trade => vec![
                    "eventType".to_string(),
                    "eventSymbol".to_string(),
                    "price".to_string(),
                    "size".to_string(),
                    "dayVolume".to_string(),
                ],
                EventType::Greeks => vec![
                    "eventType".to_string(),
                    "eventSymbol".to_string(),
                    "delta".to_string(),
                    "gamma".to_string(),
                    "theta".to_string(),
                    "vega".to_string(),
                    "rho".to_string(),
                    "volatility".to_string(),
                ],
                // Add more event types as needed
                _ => vec!["eventType".to_string(), "eventSymbol".to_string()],
            };

            accept_event_fields.insert(event_type.to_string(), fields);
        }

        let feed_setup = FeedSetupMessage {
            channel: channel_id,
            message_type: "FEED_SETUP".to_string(),
            accept_aggregation_period: 0.1,
            accept_data_format: "COMPACT".to_string(),
            accept_event_fields,
        };

        let json = serde_json::to_string(&feed_setup)?;
        debug!("Sending FEED_SETUP: {}", json);

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
            ResponseType::FeedConfig(received_channel) => {
                if received_channel != channel_id {
                    return Err(DXLinkError::Channel(format!(
                        "Expected config for channel {}, got {}",
                        channel_id, received_channel
                    )));
                }

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

        // Update internal subscriptions tracking
        {
            let mut subs = self.subscriptions.lock().unwrap();
            for sub in &subscriptions {
                subs.insert((EventType::from(sub.event_type.as_str()), sub.symbol.clone()));
            }
        }

        let subscription_msg = FeedSubscriptionMessage {
            channel: channel_id,
            message_type: "FEED_SUBSCRIPTION".to_string(),
            add: Some(subscriptions),
            remove: None,
            reset: None,
        };

        let conn = self.get_connection_mut()?;
        conn.send(&subscription_msg).await?;

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
        {
            let mut subs = self.subscriptions.lock().unwrap();
            for sub in &subscriptions {
                subs.remove(&(EventType::from(sub.event_type.as_str()), sub.symbol.clone()));
            }
        }

        let subscription_msg = FeedSubscriptionMessage {
            channel: channel_id,
            message_type: "FEED_SUBSCRIPTION".to_string(),
            add: None,
            remove: Some(subscriptions),
            reset: None,
        };

        let conn = self.get_connection_mut()?;
        conn.send(&subscription_msg).await?;

        info!("Subscriptions removed from channel {}", channel_id);

        Ok(())
    }

    /// Reset all subscriptions on a channel
    pub async fn reset_subscriptions(&mut self, channel_id: u32) -> DXLinkResult<()> {
        // Validate channel exists and is a FEED channel
        self.validate_channel(channel_id, "FEED")?;

        // Remove all subscriptions for this channel
        {
            let mut subs = self.subscriptions.lock().unwrap();
            subs.clear(); // This is a simplification - in reality you might want to track by channel
        }

        let subscription_msg = FeedSubscriptionMessage {
            channel: channel_id,
            message_type: "FEED_SUBSCRIPTION".to_string(),
            add: None,
            remove: None,
            reset: Some(true),
        };

        let conn = self.get_connection_mut()?;
        conn.send(&subscription_msg).await?;

        info!("All subscriptions reset on channel {}", channel_id);

        Ok(())
    }

    /// Close a channel
    pub async fn close_channel(&mut self, channel_id: u32) -> DXLinkResult<()> {
        // Check if the channel exists
        {
            let channels = self.channels.lock().unwrap();
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
                    let mut channels = self.channels.lock().unwrap();
                    channels.remove(&channel_id);
                }

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

    /// Register a callback function for a specific symbol
    pub fn on_event(&self, symbol: &str, callback: impl Fn(MarketEvent) + Send + Sync + 'static) {
        let mut callbacks = self.callbacks.lock().unwrap();
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
    pub fn event_stream(&mut self) -> DXLinkResult<Receiver<MarketEvent>> {
        if self.event_sender.is_none() {
            let (tx, rx) = mpsc::channel(100); // Buffer of 100 events
            self.event_sender = Some(tx);
            Ok(rx)
        } else {
            Err(DXLinkError::Protocol(
                "Event stream already created".to_string(),
            ))
        }
    }

    // Helper methods
    fn next_channel_id(&self) -> DXLinkResult<u32> {
        let mut id = self.next_channel_id.lock().unwrap();
        let channel_id = *id;
        *id += 1;
        Ok(channel_id)
    }

    fn get_connection_mut(&mut self) -> DXLinkResult<&mut WebSocketConnection> {
        self.connection
            .as_mut()
            .ok_or_else(|| DXLinkError::Connection("Not connected to DXLink server".to_string()))
    }

    fn validate_channel(&self, channel_id: u32, expected_service: &str) -> DXLinkResult<()> {
        let channels = self.channels.lock().unwrap();
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

impl fmt::Debug for DXLinkClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("DXLinkClient");

        debug_struct.field("url", &self.url);
        debug_struct.field("has_token", &(!self.token.is_empty()));
        debug_struct.field("connected", &self.connection.is_some());
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
        debug_struct.field("keepalive_active", &self.keepalive_handle.is_some());
        debug_struct.field("message_handler_active", &self.message_handle.is_some());

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
            if self.connection.is_some() {
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
            self.message_handle.is_some(),
            self.keepalive_handle.is_some(),
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
        assert!(client.connection.is_none());
        assert!(client.event_sender.is_none());
        assert!(client.keepalive_handle.is_none());
        assert!(client.message_handle.is_none());
        assert!(client.keepalive_sender.is_none());
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
        client.connection = Some(
            crate::connection::WebSocketConnection::connect(&format!("ws://{}", addr))
                .await
                .expect("failed to connect"),
        );

        client
            .start_message_processing()
            .expect("failed to start message processing");

        let handle = client
            .message_handle
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
        client.connection = Some(
            crate::connection::WebSocketConnection::connect(&format!("ws://{}", addr))
                .await
                .expect("failed to connect"),
        );

        client.start_keepalive().expect("failed to start keepalive");
        client
            .start_message_processing()
            .expect("failed to start message processing");

        let message = client.message_handle.take().expect("no message task");
        let keepalive = client.keepalive_handle.take().expect("no keepalive task");

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
