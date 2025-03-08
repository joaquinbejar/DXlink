/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use crate::connection::WebSocketConnection;
use crate::error::{DXLinkError, DXLinkResult};
use crate::events::{CompactData, EventType, MarketEvent, parse_compact_data};
use crate::messages::{
    AuthMessage, AuthStateMessage, BaseMessage, ChannelRequestMessage, ErrorMessage,
    FeedDataMessage, FeedSetupMessage, FeedSubscription, FeedSubscriptionMessage, KeepaliveMessage,
    SetupMessage,
};

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

const DEFAULT_KEEPALIVE_TIMEOUT: u32 = 60;
const DEFAULT_KEEPALIVE_INTERVAL: u32 = 15;
const DEFAULT_CLIENT_VERSION: &str = "1.0.2-dxlink-0.1.1";
const MAIN_CHANNEL: u32 = 0;

pub type EventCallback = Box<dyn Fn(MarketEvent) + Send + Sync + 'static>;

enum ResponseType {
    ChannelOpened(u32),
    FeedConfig(u32),
    ChannelClosed(u32),
    Error(String),
    #[allow(dead_code)]
    Other(String),
}

struct ResponseRequest {
    expected_type: String, // Tipo de mensaje esperado ("CHANNEL_OPENED", "FEED_CONFIG", etc.)
    channel_id: Option<u32>, // Canal esperado (None si no importa)
    response_sender: oneshot::Sender<ResponseType>,
}

pub struct DXLinkClient {
    url: String,
    token: String,
    connection: Option<WebSocketConnection>,
    keepalive_timeout: u32,
    next_channel_id: Arc<Mutex<u32>>,
    channels: Arc<Mutex<HashMap<u32, String>>>, // channel_id -> service
    callbacks: Arc<Mutex<HashMap<String, EventCallback>>>, // symbol -> callback
    subscriptions: Arc<Mutex<HashSet<(EventType, String)>>>, // (event_type, symbol)
    event_sender: Option<Sender<MarketEvent>>,
    keepalive_handle: Option<JoinHandle<()>>,
    message_handle: Option<JoinHandle<()>>,
    keepalive_sender: Option<Sender<()>>,
    response_requests: Arc<Mutex<Vec<ResponseRequest>>>,
}

impl DXLinkClient {
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

    /// Connect to the DXLink server and perform the setup and authentication
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
