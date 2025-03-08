/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents a basic message structure.
///
/// This struct is used for communication, defining a channel and the type of message.
/// The `serde` attributes enable serialization and deserialization in camelCase format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseMessage {
    /// The channel number.
    pub channel: u32,
    /// The type of the message.
    #[serde(rename = "type")]
    pub message_type: String,
}

/// Represents a setup message for establishing a connection.
///
/// This message is used to initiate a connection and exchange
/// initial setup parameters between client and server.  It includes
/// information such as the channel number, message type, keepalive
/// timeout values, and the version of the protocol being used.
///
/// # Examples
///
/// Serializing a `SetupMessage`:
///
/// ```rust
/// use serde_json::json;
/// use dxlink::messages::SetupMessage;
///
/// let setup_message = SetupMessage {
///     channel: 1,
///     message_type: "setup".to_string(),
///     keepalive_timeout: 30000,
///     accept_keepalive_timeout: 35000,
///     version: "1.0".to_string(),
/// };
///
/// let json_representation = serde_json::to_string(&setup_message).unwrap();
/// assert_eq!(json_representation, r#"{"channel":1,"type":"setup","keepaliveTimeout":30000,"acceptKeepaliveTimeout":35000,"version":"1.0"}"#);
///
/// // You can also create it from a JSON string.
/// let setup_message: SetupMessage = serde_json::from_value(json!({
///     "channel": 1,
///     "type": "setup",
///     "keepaliveTimeout": 30000,
///     "acceptKeepaliveTimeout": 35000,
///     "version": "1.0"
/// })).unwrap();
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupMessage {
    /// The channel number.
    pub channel: u32,
    /// The type of the message.  Should be "setup".
    #[serde(rename = "type")]
    pub message_type: String,
    /// The keepalive timeout value in milliseconds.
    pub keepalive_timeout: u32,
    /// The timeout value for accepting a keepalive message.
    pub accept_keepalive_timeout: u32,
    /// The version of the protocol.
    pub version: String,
}

/// Represents a keepalive message.  This message is used to maintain an active connection
/// and prevent timeouts.  It is sent periodically by the client or server.
///
/// # Example:
///
/// ```json
/// {
///   "channel": 1234,
///   "type": "KEEPALIVE"
/// }
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepaliveMessage {
    /// The channel ID.
    pub channel: u32,
    /// The message type.  Should always be "KEEPALIVE".
    #[serde(rename = "type")]
    pub message_type: String,
}

/// Represents an authentication message.
///
/// This structure is used for authentication purposes, containing information such as the channel, message type, and token.
/// The `rename_all = "camelCase"` attribute ensures that the fields are serialized and deserialized in camel case format.
///
/// ...
/// # Examples
///
/// Serializing an `AuthMessage` instance to JSON:
///
/// ```rust
/// use serde::{Deserialize, Serialize};
/// use serde_json::to_string;
/// use dxlink::messages::AuthMessage;
///
/// let auth_message = AuthMessage {
///     channel: 1234,
///     message_type: "auth".to_string(),
///     token: "YOUR_TOKEN".to_string(),
/// };
///
/// let json_string = to_string(&auth_message).unwrap();
/// println!("{}", json_string);
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMessage {
    /// The channel number.
    pub channel: u32,
    /// The type of the message. This is typically "auth".
    #[serde(rename = "type")]
    pub message_type: String,
    /// The authentication token.
    pub token: String,
}

/// Represents an authentication state message.  This structure is used for serializing
/// and deserializing authentication state messages, typically used in a
/// communication channel like a WebSocket. The `camelCase` serialization
/// attribute ensures compatibility with JavaScript conventions.
///
/// # Examples
///
/// Serialization:
///
/// ```rust
/// use serde_json;
/// use dxlink::messages::AuthStateMessage;
///
/// let message = AuthStateMessage {
///     channel: 1234,
///     message_type: "authState".to_string(),
///     state: "authenticated".to_string(),
///     user_id: Some("user123".to_string()),
/// };
///
/// let json_string = serde_json::to_string(&message).unwrap();
/// println!("{}", json_string); // Output: {"channel":1234,"type":"authState","state":"authenticated","userId":"user123"}
/// ```
///
/// Deserialization:
///
/// ```rust
/// use serde_json;
/// use dxlink::messages::AuthStateMessage;
///
/// let json_string = r#"{"channel":1234,"type":"authState","state":"authenticated","userId":"user123"}"#;
/// let message: AuthStateMessage = serde_json::from_str(json_string).unwrap();
///
/// assert_eq!(message.channel, 1234);
/// assert_eq!(message.message_type, "authState");
/// assert_eq!(message.state, "authenticated");
/// assert_eq!(message.user_id, Some("user123".to_string()));
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStateMessage {
    /// The channel number.
    pub channel: u32,
    /// The type of the message.  Typically "authState".
    #[serde(rename = "type")]
    pub message_type: String,
    /// The authentication state (e.g., "authenticated", "unauthenticated").
    pub state: String,
    /// The ID of the user.  Optional.
    pub user_id: Option<String>,
}

/// Represents a channel request message.
///
/// This structure is used to send requests to a specific channel.  It includes the channel number,
/// the type of message, the service being requested, and any associated parameters.  Serialization and
/// deserialization are handled using `serde` with `camelCase` naming convention.
///
/// # Examples
///
/// ```rust
/// use std::collections::HashMap;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Serialize, Deserialize)]
/// #[serde(rename_all = "camelCase")]
/// pub struct ChannelRequestMessage {
///     pub channel: u32,
///     #[serde(rename = "type")]
///     pub message_type: String,
///     pub service: String,
///     pub parameters: HashMap<String, String>,
/// }
///
/// let mut parameters = HashMap::new();
/// parameters.insert("param1".to_string(), "value1".to_string());
/// parameters.insert("param2".to_string(), "value2".to_string());
///
/// let message = ChannelRequestMessage {
///     channel: 123,
///     message_type: "request".to_string(),
///     service: "my_service".to_string(),
///     parameters: parameters,
/// };
///
/// let serialized = serde_json::to_string(&message).unwrap();
/// println!("{}", serialized);
///
/// let deserialized: ChannelRequestMessage = serde_json::from_str(&serialized).unwrap();
/// println!("{:?}", deserialized);
///
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRequestMessage {
    /// The channel number.
    pub channel: u32,
    /// The type of the message.
    #[serde(rename = "type")]
    pub message_type: String,
    /// The service being requested.
    pub service: String,
    /// The parameters associated with the request.
    pub parameters: HashMap<String, String>,
}

/// Represents a CHANNEL_OPENED message.  This message is sent when a new channel
/// is opened.
///
/// # Example
///
/// ```json
/// {
///   "channel": 123,
///   "type": "CHANNEL_OPENED",
///   "service": "some_service",
///   "parameters": {
///     "param1": "value1",
///     "param2": "value2"
///   }
/// }
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelOpenedMessage {
    /// The channel number.
    pub channel: u32,
    /// The message type.  This should always be "CHANNEL_OPENED".
    #[serde(rename = "type")]
    pub message_type: String,
    /// The service associated with the channel.  This is optional.
    #[serde(default)]
    pub service: Option<String>,
    /// Additional parameters associated with the channel opening.  This is optional.
    #[serde(default)]
    pub parameters: HashMap<String, String>,
}

/// Represents a message indicating a channel has been closed.
///
/// This message is typically used in scenarios where a communication channel,
/// identified by a numerical ID, is closed.  The `message_type` field
/// clarifies the type of message, explicitly set to "CHANNEL_CLOSED".
///
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelClosedMessage {
    /// The ID of the channel that has been closed.
    pub channel: u32,
    /// The type of the message.  This field will always be "CHANNEL_CLOSED".
    #[serde(rename = "type")]
    pub message_type: String,
}

/// Represents a message to cancel a channel.
///
/// This message is used to signal the cancellation of a specific channel.  It includes the channel number and the message type.
///
/// # Examples
///
/// ```
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Serialize, Deserialize)]
/// #[serde(rename_all = "camelCase")]
/// pub struct ChannelCancelMessage {
///     pub channel: u32,
///     #[serde(rename = "type")]
///     pub message_type: String,
/// }
///
/// let message = ChannelCancelMessage {
///     channel: 123,
///     message_type: "CHANNEL_CANCEL".to_string(),
/// };
///
/// // Serialize the message to JSON
/// let json = serde_json::to_string(&message).unwrap();
///
/// // Deserialize the message from JSON
/// let deserialized_message: ChannelCancelMessage = serde_json::from_str(&json).unwrap();
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelCancelMessage {
    /// The number of the channel to cancel.
    pub channel: u32,
    /// The type of the message.  This should always be "CHANNEL_CANCEL".
    #[serde(rename = "type")]
    pub message_type: String,
}

/// Represents an error message.
///
/// This struct is used to serialize and deserialize error messages in a structured format.
/// It adheres to the camelCase naming convention for serialization/deserialization thanks to the `#[serde(rename_all = "camelCase")]` attribute.
///
/// # Examples
///
/// ```
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Serialize, Deserialize)]
/// #[serde(rename_all = "camelCase")]
/// pub struct ErrorMessage {
///     pub channel: u32,
///     #[serde(rename = "type")]
///     pub message_type: String,
///     pub error: String,
///     pub message: String,
/// }
///
/// let error_message = ErrorMessage {
///     channel: 1,
///     message_type: "error".to_string(),
///     error: "ErrorCode".to_string(),
///     message: "Something went wrong".to_string(),
/// };
///
/// let serialized = serde_json::to_string(&error_message).unwrap();
///
/// assert_eq!(serialized, r#"{"channel":1,"type":"error","error":"ErrorCode","message":"Something went wrong"}"#);
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorMessage {
    /// The channel the error occurred on.
    pub channel: u32,
    /// The type of the message, which should be "error".
    #[serde(rename = "type")]
    pub message_type: String,
    /// A short error code.
    pub error: String,
    /// A human-readable error message.
    pub message: String,
}

/// Represents a subscription to a data feed.
///
/// This struct is used to specify the type of data, the symbol,
/// optional filtering by time, and the optional source of the data.
/// It is serialized and deserialized using the `serde` library,
/// with field names converted to camelCase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSubscription {
    /// The type of event to subscribe to.  For example, "trade", "kline", etc.
    #[serde(rename = "type")]
    pub event_type: String,

    /// The symbol to subscribe to. For example, "BTCUSDT".
    pub symbol: String,

    /// Optional starting time for the subscription, represented as a Unix timestamp in milliseconds.
    /// If not provided, the subscription will start from the current time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_time: Option<i64>,

    /// Optional source of the data.  This can be used to specify a particular
    /// exchange or data provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Represents a message for managing feed subscriptions.
///
/// This message is used to add, remove, or reset subscriptions to data feeds.
/// It is serialized and deserialized using the `serde` library,
/// with field names converted to camelCase.  The `channel` field is used
/// to identify the specific connection or channel the message is associated with.
/// The `type` field indicates the type of message, which is always "FEED_SUBSCRIPTION".
/// The `add`, `remove`, and `reset` fields are optional and mutually exclusive.
/// If `add` is present, it contains a vector of `FeedSubscription` objects to be added.
/// If `remove` is present, it contains a vector of `FeedSubscription` objects to be removed.
/// If `reset` is true, all existing subscriptions are removed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSubscriptionMessage {
    /// The channel ID.
    pub channel: u32,

    /// The message type.  This should always be "FEED_SUBSCRIPTION".
    #[serde(rename = "type")]
    pub message_type: String,

    /// An optional vector of subscriptions to add.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<FeedSubscription>>,

    /// An optional vector of subscriptions to remove.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<FeedSubscription>>,

    /// An optional flag to reset all subscriptions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset: Option<bool>,
}

/// Represents the setup message for a data feed.  This message is used to configure the channel, message type,
/// aggregation period, data format, and accepted event fields for the feed.
///
/// # Example
///
/// ```json
/// {
///   "channel": 1234,
///   "type": "marketData",
///   "acceptAggregationPeriod": 60.0,
///   "acceptDataFormat": "json",
///   "acceptEventFields": {
///     "trade": ["price", "quantity"],
///     "quote": ["bid", "ask"]
///   }
/// }
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSetupMessage {
    /// The channel identifier for the feed.
    pub channel: u32,

    /// The type of message expected on the feed.  For example, "marketData", "orderEvents", etc.
    #[serde(rename = "type")]
    pub message_type: String,

    /// The accepted aggregation period for the feed, in seconds.  This indicates how frequently aggregated
    /// data should be sent.  If not applicable, a value of 0.0 can be used.
    pub accept_aggregation_period: f64,

    /// The accepted data format for the feed.  For example, "json", "csv", "protobuf", etc.
    pub accept_data_format: String,

    /// A map of event types to a list of accepted fields for each type.  This allows for fine-grained control
    /// over the data received on the feed.  For example, for a "trade" event, you might only want to receive
    /// the "price" and "quantity" fields.
    pub accept_event_fields: HashMap<String, Vec<String>>,
}

///
/// Represents the configuration for a feed message.
///
/// This structure defines how data should be aggregated and formatted for a specific channel.
/// It includes details like the channel number, message type, aggregation period, data format, and optional event fields.
///
/// # Examples
///
/// ```json
/// {
///   "channel": 123,
///   "type": "marketData",
///   "aggregationPeriod": 60.0,
///   "dataFormat": "json",
///   "eventFields": {
///     "trade": ["price", "volume"],
///     "quote": ["bid", "ask"]
///   }
/// }
/// ```
///
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedConfigMessage {
    /// The channel number for the feed.
    pub channel: u32,

    /// The type of the message.  For example "marketData", or "orderEvents".
    #[serde(rename = "type")]
    pub message_type: String,

    /// The aggregation period in seconds. Data will be aggregated over this time interval.
    pub aggregation_period: f64,

    /// The format of the data. For example "json", "csv", or "protobuf".
    pub data_format: String,

    /// Optional event fields to include in the message. This is a map where the keys are event types
    /// and the values are vectors of field names to include for that event type.
    #[serde(default)]
    pub event_fields: Option<HashMap<String, Vec<String>>>,
}

/// Represents a message containing feed data.
///
/// This struct is used to serialize and deserialize feed data messages,
/// adhering to a camelCase naming convention for JSON serialization.
///
/// `T` represents the type of the data being transmitted in the message.  
///
/// # Examples
///
/// ```rust
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Serialize, Deserialize)]
/// #[serde(rename_all = "camelCase")]
/// pub struct FeedDataMessage<T> {
///     pub channel: u32,
///     #[serde(rename = "type")]
///     pub message_type: String,
///     pub data: T,
/// }
///
/// #[derive(Debug, Serialize, Deserialize)]
/// pub struct MyData {
///     value: i32,
/// }
///
/// let message = FeedDataMessage {
///     channel: 123,
///     message_type: "data".to_string(),
///     data: MyData { value: 42 },
/// };
///
/// let json = serde_json::to_string(&message).unwrap();
///
/// println!("{}", json); // Output: {"channel":123,"type":"data","data":{"value":42}}
/// ```
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedDataMessage<T> {
    /// The channel number associated with the message.
    pub channel: u32,
    /// The type of the message.  This field is renamed to "type" during serialization.
    #[serde(rename = "type")]
    pub message_type: String,
    /// The actual data being transmitted in the message.  This can be any serializable type.
    pub data: T,
}
