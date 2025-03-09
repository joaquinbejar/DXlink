/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use crate::connection::WebSocketConnection;
use crate::error::{DXLinkError, DXLinkResult};
use crate::events::{CompactData, EventType, MarketEvent};
use crate::messages::{
    AuthMessage, AuthStateMessage, BaseMessage, ChannelRequestMessage, ErrorMessage,
    FeedDataMessage, FeedSetupMessage, FeedSubscription, FeedSubscriptionMessage, KeepaliveMessage,
    SetupMessage,
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

/// Default interval for sending keep-alive messages in seconds.  Clients should
/// send a keep-alive message at least this often to maintain the connection.
const DEFAULT_KEEPALIVE_INTERVAL: u32 = 15;

/// Default client version string.  This is used to identify the client
/// software version to the server.
const DEFAULT_CLIENT_VERSION: &str = "1.0.2-dxlink-0.1.3";

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
    /// Indicates an error has occurred. The `String` value contains the error message.
    Error(String),
    /// A generic response type for other cases. The `String` value contains the response data.  This variant is currently unused (`#[allow(dead_code)]`).
    #[allow(dead_code)]
    Other(String),
}

/// Represents a request for a specific response from a WebSocket stream.  This struct is used to await a particular
/// response type, optionally filtered by channel ID.  It includes a `oneshot::Sender` to send the
/// response back to the requester.
#[derive(Debug)]
struct ResponseRequest {
    /// The expected type of the response message (e.g., "CHANNEL_OPENED", "FEED_CONFIG", etc.).  This string should match the expected
    /// response message type.
    expected_type: String,
    /// The expected channel ID for the response.  If `None`, the channel ID is not considered when matching responses.
    channel_id: Option<u32>,
    /// A `oneshot::Sender` used to send the `ResponseType` back to the requester once the expected response is received.
    response_sender: oneshot::Sender<ResponseType>,
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
///    as an `Option<WebSocketConnection>`, where `None` indicates no active connection.
/// * `keepalive_timeout`: The timeout for keepalive messages in seconds.
/// * `next_channel_id`: A thread-safe counter for generating unique channel IDs.  It's
///    wrapped in an `Arc<Mutex>` to allow shared access across multiple threads.
/// * `channels`: A thread-safe map that stores the association between channel IDs and
///    the services they are subscribed to.  This is also wrapped in an `Arc<Mutex>`
///    for thread safety.
/// * `callbacks`: A thread-safe map that stores callback functions associated with
///    specific market data symbols.  The callbacks are of type `EventCallback`,
///    which are functions that process incoming `MarketEvent` data.  An `Arc<Mutex>`
///    is used for thread safety.
/// * `subscriptions`: A thread-safe set that keeps track of active subscriptions,
///    identified by pairs of `EventType` and the corresponding market data symbol.
///    This ensures that duplicate subscriptions are avoided and allows for efficient
///    management of subscriptions.  It uses `Arc<Mutex>` for thread safety.
/// * `event_sender`: A sender for transmitting `MarketEvent` instances.  This is
///    optional (`Option<Sender<MarketEvent>>`) and is used to relay events to
///    internal processing or external consumers.
/// * `keepalive_handle`: A handle to the keepalive task.  The keepalive task
///    periodically sends messages to the server to maintain the connection.
///    This is an `Option<JoinHandle<()>>` which represents a potentially running
///    background task.
/// * `message_handle`: A handle to the message processing task. The message
///    processing task is responsible for receiving and handling incoming WebSocket
///    messages.  This is stored as an `Option<JoinHandle<()>>` to manage the
///    background task's lifecycle.
/// * `keepalive_sender`:  A channel sender used to signal the keepalive task.
///    This is of type `Option<Sender<()>>`, which may be used to control
///    or stop the keepalive task.
/// * `response_requests`: A thread-safe vector that holds pending response requests.
///    This is used to manage asynchronous responses from the server and is wrapped
///    in an `Arc<Mutex>` for thread safety.
pub struct DXLinkClient {
    /// The URL of the DXLink WebSocket server.
    url: String,
    /// The authentication token for accessing the DXLink service.
    token: String,
    /// The active WebSocket connection, if established.  `None` indicates no active connection.
    connection: Option<WebSocketConnection>,
    /// The timeout for keepalive messages in seconds.
    keepalive_timeout: u32,
    /// A thread-safe counter for generating unique channel IDs.
    next_channel_id: Arc<Mutex<u32>>,
    /// A thread-safe map storing the association between channel IDs and the services they are subscribed to.
    channels: Arc<Mutex<HashMap<u32, String>>>, // channel_id -> service
    /// A thread-safe map storing callback functions associated with specific market data symbols.
    callbacks: Arc<Mutex<HashMap<String, EventCallback>>>, // symbol -> callback
    /// A thread-safe set keeping track of active subscriptions, identified by `(EventType, String)`.
    subscriptions: Arc<Mutex<HashSet<(EventType, String)>>>, // (event_type, symbol)
    /// A sender for transmitting `MarketEvent` instances.
    event_sender: Option<Sender<MarketEvent>>,
    /// A handle to the keepalive task.
    keepalive_handle: Option<JoinHandle<()>>,
    /// A handle to the message processing task.
    message_handle: Option<JoinHandle<()>>,
    /// A channel sender used to signal the keepalive task.
    keepalive_sender: Option<Sender<()>>,
    /// A thread-safe vector that holds pending response requests.
    response_requests: Arc<Mutex<Vec<ResponseRequest>>>,
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
            next_channel_id: Arc::new(Mutex::new(1)), // Start from 1 as 0 is the main channel
            channels: Arc::new(Mutex::new(HashMap::new())),
            callbacks: Arc::new(Mutex::new(HashMap::new())),
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
            event_sender: None,
            keepalive_handle: None,
            message_handle: None,
            keepalive_sender: None,
            response_requests: Arc::new(Mutex::new(Vec::new())),
        }
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
    /// ```rust,ignore
    /// # use dxlink::client::DXLinkClient;
    /// # use dxlink::error::DXLinkResult;
    /// # #[tokio::main]
    /// # async fn main() -> DXLinkResult<()> {
    /// let mut client = DXLinkClient::new("ws://your_dxlink_server_url", "YOUR_TOKEN", 30000)?;
    /// client.connect().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(&mut self) -> DXLinkResult<()> {
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

        // Receive SETUP response
        let response = connection.receive().await?;
        let _: SetupMessage = serde_json::from_str(&response)?;

        // Check for AUTH_STATE message
        let response = connection.receive().await?;
        let auth_state: AuthStateMessage = serde_json::from_str(&response)?;

        // Si ya estamos autorizados, podemos omitir el proceso de autenticación
        if auth_state.state == "AUTHORIZED" {
            info!("Already authorized to DXLink server");
        } else if auth_state.state == "UNAUTHORIZED" {
            // Send AUTH message
            let auth_msg = AuthMessage {
                channel: MAIN_CHANNEL,
                message_type: "AUTH".to_string(),
                token: self.token.clone(),
            };

            connection.send(&auth_msg).await?;

            // Receive AUTH_STATE response, should be AUTHORIZED
            let response = connection.receive().await?;
            let auth_state: AuthStateMessage = serde_json::from_str(&response)?;

            if auth_state.state != "AUTHORIZED" {
                return Err(DXLinkError::Authentication(format!(
                    "Authentication failed. State: {}",
                    auth_state.state
                )));
            }

            info!("Successfully authenticated to DXLink server");
        } else {
            return Err(DXLinkError::Protocol(format!(
                "Unexpected authentication state: {}",
                auth_state.state
            )));
        }

        info!("Successfully connected to DXLink server");

        self.connection = Some(connection);

        // Start message processing task first so it puede capturar todos los mensajes
        self.start_message_processing()?;

        // Start keepalive task with a channel
        self.start_keepalive()?;

        Ok(())
    }

    /// Waits for a specific response type from the DXLink device, optionally filtered by channel ID.
    ///
    /// This function registers a request for a specific response type and then waits for the corresponding
    /// response to be received from the DXLink device.  The wait is subject to a timeout.
    ///
    /// # Arguments
    ///
    /// * `expected_type` - The expected type of the response message (e.g., "CHANNEL_OPENED", "FEED_CONFIG", etc.).
    /// * `channel_id` - The expected channel ID for the response.  If `None`, any channel ID is accepted.
    /// * `timeout` - The maximum time to wait for the response.
    ///
    /// # Returns
    ///
    /// * `Ok(ResponseType)` - If the expected response is received within the timeout period.
    /// * `Err(DXLinkError::Timeout)` - If the timeout period expires before the expected response is received.
    /// * `Err(DXLinkError::Protocol)` - If the response channel is closed unexpectedly.
    ///
    #[allow(dead_code)]
    async fn wait_for_response(
        &self,
        expected_type: &str,
        channel_id: Option<u32>,
        timeout: Duration,
    ) -> DXLinkResult<ResponseType> {
        let (tx, rx) = oneshot::channel();

        // Registrar nuestra solicitud de respuesta
        {
            let mut requests = self.response_requests.lock().unwrap();
            requests.push(ResponseRequest {
                expected_type: expected_type.to_string(),
                channel_id,
                response_sender: tx,
            });
        }

        // Esperar la respuesta con timeout
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(DXLinkError::Protocol("Response channel closed".to_string())),
            Err(_) => Err(DXLinkError::Timeout(format!(
                "Timed out waiting for {} message{}",
                expected_type,
                channel_id.map_or("".to_string(), |id| format!(" for channel {}", id))
            ))),
        }
    }

    /// Starts the keepalive task.
    ///
    /// This function spawns a new tokio task that periodically sends keepalive messages
    /// to the DXLink device.  The interval between keepalive messages is defined by
    /// the `DEFAULT_KEEPALIVE_INTERVAL` constant.
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

        // Usar la constante para el intervalo de keepalive
        let keepalive_interval = Duration::from_secs(DEFAULT_KEEPALIVE_INTERVAL as u64);

        let keepalive_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(keepalive_interval);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        // Es hora de enviar un keepalive
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
                                // Salir del bucle en caso de error para que la tarea termine
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
        // Asegurarnos de que tenemos una conexión
        if self.connection.is_none() {
            return Err(DXLinkError::Connection(
                "Cannot start message processing without a connection".to_string(),
            ));
        }

        // Clonar la conexión para usar en la tarea
        let connection = self.connection.as_ref().unwrap().clone();

        // Clonar referencias que necesitamos
        let callbacks = self.callbacks.clone();
        let event_sender = self.event_sender.clone();
        let response_requests = self.response_requests.clone();

        // Iniciar la tarea de procesamiento de mensajes
        let message_handle = tokio::spawn(async move {
            loop {
                match connection.receive().await {
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

                            // Primero, comprobar si alguien está esperando este mensaje
                            {
                                let mut requests = response_requests.lock().unwrap();
                                if let Some(idx) = requests.iter().position(|req| {
                                    req.expected_type == msg_type
                                        && (req.channel_id.is_none() || req.channel_id == channel)
                                }) {
                                    // Encontramos alguien esperando este mensaje
                                    let request = requests.remove(idx);

                                    // Crear la respuesta apropiada
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
                                            ResponseType::Error(format!("{} - {}", error, message))
                                        }
                                        _ => ResponseType::Other(msg.clone()),
                                    };

                                    // Enviar la respuesta (ignorando errores si el receptor ya no existe)
                                    let _ = request.response_sender.send(response);
                                    continue; // Pasar al siguiente mensaje
                                }
                            }

                            // Si nadie esperaba este mensaje específicamente, procesarlo normalmente
                            match msg_type {
                                "FEED_DATA" => {
                                    if let Ok(data_msg) = serde_json::from_str::<
                                        FeedDataMessage<Vec<CompactData>>,
                                    >(&msg)
                                    {
                                        let events = parse_compact_data(&data_msg.data);
                                        for event in events {
                                            let symbol = match &event {
                                                MarketEvent::Quote(e) => &e.event_symbol,
                                                MarketEvent::Trade(e) => &e.event_symbol,
                                                MarketEvent::Greeks(e) => &e.event_symbol,
                                            };

                                            // Enviarlo a los callbacks
                                            if let Ok(callbacks) = callbacks.lock() {
                                                if let Some(callback) = callbacks.get(symbol) {
                                                    callback(event.clone());
                                                }
                                            }

                                            // Enviarlo al canal de eventos
                                            if let Some(tx) = &event_sender {
                                                if let Err(e) = tx.send(event.clone()).await {
                                                    error!(
                                                        "Failed to send event to channel: {}",
                                                        e
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
                    Err(e) => {
                        error!("Error receiving message: {}", e);
                        // Una pequeña pausa para no saturar logs en caso de errores repetidos
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
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

        // Registrar nuestra expectativa de respuesta
        let (tx, rx) = oneshot::channel();
        {
            let mut requests = self.response_requests.lock().unwrap();
            requests.push(ResponseRequest {
                expected_type: "CHANNEL_OPENED".to_string(),
                channel_id: Some(channel_id),
                response_sender: tx,
            });
        }

        // Enviar la solicitud
        let conn = self.get_connection_mut()?;
        conn.send(&channel_request).await?;

        // Esperar la respuesta
        let response = match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err(DXLinkError::Protocol("Response channel closed".to_string())),
            Err(_) => {
                return Err(DXLinkError::Timeout(format!(
                    "Timed out waiting for CHANNEL_OPENED message for channel {}",
                    channel_id
                )));
            }
        };

        // Procesar la respuesta
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
            ResponseType::Error(error) => Err(DXLinkError::Protocol(format!(
                "Server returned error: {}",
                error
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

        // Registrar nuestra expectativa de respuesta
        let (tx, rx) = oneshot::channel();
        {
            let mut requests = self.response_requests.lock().unwrap();
            requests.push(ResponseRequest {
                expected_type: "FEED_CONFIG".to_string(),
                channel_id: Some(channel_id),
                response_sender: tx,
            });
        }

        // Enviar la solicitud
        let conn = self.get_connection_mut()?;
        conn.send(&feed_setup).await?;

        // Esperar la respuesta
        let response = match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err(DXLinkError::Protocol("Response channel closed".to_string())),
            Err(_) => {
                return Err(DXLinkError::Timeout(format!(
                    "Timed out waiting for FEED_CONFIG message for channel {}",
                    channel_id
                )));
            }
        };

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
            ResponseType::Error(error) => Err(DXLinkError::Protocol(format!(
                "Server returned error: {}",
                error
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

        // Registrar nuestra expectativa de respuesta (sin retener un futuro todavía)
        let (tx, rx) = oneshot::channel();
        {
            let mut requests = self.response_requests.lock().unwrap();
            requests.push(ResponseRequest {
                expected_type: "CHANNEL_CLOSED".to_string(),
                channel_id: Some(channel_id),
                response_sender: tx,
            });
        }

        // Ahora podemos obtener la conexión mutable y enviar
        let conn = self.get_connection_mut()?;
        conn.send(&cancel_msg).await?;

        // Esperar la respuesta con timeout
        let response = match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return Err(DXLinkError::Protocol("Response channel closed".to_string())),
            Err(_) => {
                return Err(DXLinkError::Timeout(format!(
                    "Timed out waiting for CHANNEL_CLOSED message for channel {}",
                    channel_id
                )));
            }
        };

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
            ResponseType::Error(error) => Err(DXLinkError::Protocol(format!(
                "Server returned error: {}",
                error
            ))),
            _ => Err(DXLinkError::Protocol(
                "Unexpected response type".to_string(),
            )),
        }
    }

    /// Register a callback function for a specific symbol
    pub fn on_event(&self, symbol: &str, callback: impl Fn(MarketEvent) + Send + Sync + 'static) {
        let mut callbacks = self.callbacks.lock().unwrap();
        callbacks.insert(symbol.to_string(), Box::new(callback));
    }

    /// Get a stream of market events
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
