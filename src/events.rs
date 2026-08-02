/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use serde::{Deserialize, Serialize};
use std::fmt;

/// Represents different types of events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    /// Quote event.
    Quote,
    /// Trade event.
    Trade,
    /// Summary event.
    Summary,
    /// Profile event.
    Profile,
    /// Order event.
    Order,
    /// Time and Sale event.
    TimeAndSale,
    /// Candle event.
    Candle,
    /// TradeETH event.
    TradeETH,
    /// Spread Order event.
    SpreadOrder,
    /// Greeks event.
    Greeks,
    /// Theoretical Price event.
    TheoPrice,
    /// Underlying event.
    Underlying,
    /// Series event.
    Series,
    /// Configuration event.
    Configuration,
    /// Message event.
    Message,
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            EventType::Quote => write!(f, "Quote"),
            EventType::Trade => write!(f, "Trade"),
            EventType::Summary => write!(f, "Summary"),
            EventType::Profile => write!(f, "Profile"),
            EventType::Order => write!(f, "Order"),
            EventType::TimeAndSale => write!(f, "TimeAndSale"),
            EventType::Candle => write!(f, "Candle"),
            EventType::TradeETH => write!(f, "TradeETH"),
            EventType::SpreadOrder => write!(f, "SpreadOrder"),
            EventType::Greeks => write!(f, "Greeks"),
            EventType::TheoPrice => write!(f, "TheoPrice"),
            EventType::Underlying => write!(f, "Underlying"),
            EventType::Series => write!(f, "Series"),
            EventType::Configuration => write!(f, "Configuration"),
            EventType::Message => write!(f, "Message"),
        }
    }
}

/// Parses a wire name, answering `Quote` for anything it does not recognise.
///
/// **This conversion loses information and should not be used on a protocol
/// path.** A typo such as `"Qutoe"` becomes `Quote`, so the client records a
/// subscription it never made while sending the misspelling to the server.
/// Use [`EventType::from_str`] or [`EventType::from_wire_name`] instead, both
/// of which say they do not know the name.
///
/// It is kept for source compatibility and is scheduled for removal in the
/// next minor release of the 0.x line; nothing inside this crate uses it.
impl From<&str> for EventType {
    fn from(s: &str) -> Self {
        match s {
            "Quote" => EventType::Quote,
            "Trade" => EventType::Trade,
            "Summary" => EventType::Summary,
            "Profile" => EventType::Profile,
            "Order" => EventType::Order,
            "TimeAndSale" => EventType::TimeAndSale,
            "Candle" => EventType::Candle,
            "TradeETH" => EventType::TradeETH,
            "SpreadOrder" => EventType::SpreadOrder,
            "Greeks" => EventType::Greeks,
            "TheoPrice" => EventType::TheoPrice,
            "Underlying" => EventType::Underlying,
            "Series" => EventType::Series,
            "Configuration" => EventType::Configuration,
            "Message" => EventType::Message,
            _ => EventType::Quote, // Default
        }
    }
}

/// Every event type the protocol declares.
///
/// Written out by hand, which an added variant does **not** force anyone to
/// update — the array still compiles while missing it. What catches that is the
/// round-trip test below, which walks this list and would stop covering a new
/// variant, so the list and the test have to be updated together.
pub const ALL_EVENT_TYPES: [EventType; 15] = [
    EventType::Quote,
    EventType::Trade,
    EventType::Summary,
    EventType::Profile,
    EventType::Order,
    EventType::TimeAndSale,
    EventType::Candle,
    EventType::TradeETH,
    EventType::SpreadOrder,
    EventType::Greeks,
    EventType::TheoPrice,
    EventType::Underlying,
    EventType::Series,
    EventType::Configuration,
    EventType::Message,
];

impl std::str::FromStr for EventType {
    type Err = crate::DXLinkError;

    /// Parses a wire name, rejecting anything the protocol does not declare.
    ///
    /// # Errors
    ///
    /// [`DXLinkError::Protocol`](crate::DXLinkError::Protocol) naming the
    /// string that was not recognised.
    ///
    /// # Example
    ///
    /// ```rust
    /// use dxlink::EventType;
    ///
    /// assert_eq!("Quote".parse::<EventType>().unwrap(), EventType::Quote);
    /// // The lenient From impl would answer Quote for this.
    /// assert!("Qutoe".parse::<EventType>().is_err());
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EventType::from_wire_name(s).ok_or_else(|| {
            crate::DXLinkError::Protocol(format!(
                "`{s}` is not a DXLink event type; the protocol declares Quote, Trade, \
                 Summary, Profile, Order, TimeAndSale, Candle, TradeETH, SpreadOrder, \
                 Greeks, TheoPrice, Underlying, Series, Configuration and Message"
            ))
        })
    }
}

impl EventType {
    /// Parses a wire name, answering `None` for anything unknown.
    ///
    /// Unlike the [`From<&str>`] impl, which answers `Quote` for any
    /// unrecognised name, this reports that it does not know it. A decoder
    /// cannot use the lenient conversion: silently treating an unknown type as
    /// `Quote` means reading its row with the wrong layout.
    pub fn from_wire_name(value: &str) -> Option<Self> {
        match value {
            "Quote" => Some(EventType::Quote),
            "Trade" => Some(EventType::Trade),
            "Summary" => Some(EventType::Summary),
            "Profile" => Some(EventType::Profile),
            "Order" => Some(EventType::Order),
            "TimeAndSale" => Some(EventType::TimeAndSale),
            "Candle" => Some(EventType::Candle),
            "TradeETH" => Some(EventType::TradeETH),
            "SpreadOrder" => Some(EventType::SpreadOrder),
            "Greeks" => Some(EventType::Greeks),
            "TheoPrice" => Some(EventType::TheoPrice),
            "Underlying" => Some(EventType::Underlying),
            "Series" => Some(EventType::Series),
            "Configuration" => Some(EventType::Configuration),
            "Message" => Some(EventType::Message),
            _ => None,
        }
    }

    /// The COMPACT field list this client requests for the event type, in the
    /// exact order the decoder reads it.
    ///
    /// This is what `setup_feed` requests and what the `FEED_CONFIG` reply is
    /// validated against. The decoder in `utils.rs` still carries its own
    /// positions, so the contract is not yet enforced from one definition —
    /// moving the decoder onto this list is the next step.
    ///
    /// `None` means this client has no decoder for the type: it can be named,
    /// but no row can be turned into a [`MarketEvent`](crate::MarketEvent).
    pub fn compact_fields(&self) -> Option<&'static [&'static str]> {
        match self {
            EventType::Quote => Some(&[
                "eventType",
                "eventSymbol",
                "bidPrice",
                "askPrice",
                "bidSize",
                "askSize",
            ]),
            EventType::Trade => Some(&["eventType", "eventSymbol", "price", "size", "dayVolume"]),
            EventType::Candle => Some(&[
                "eventType",
                "eventSymbol",
                "time",
                "open",
                "high",
                "low",
                "close",
                "volume",
            ]),
            EventType::Greeks => Some(&[
                "eventType",
                "eventSymbol",
                "delta",
                "gamma",
                "theta",
                "vega",
                "rho",
                "volatility",
            ]),
            // Declared by the protocol, not decoded here.
            EventType::Summary
            | EventType::Profile
            | EventType::Order
            | EventType::TimeAndSale
            | EventType::TradeETH
            | EventType::SpreadOrder
            | EventType::TheoPrice
            | EventType::Underlying
            | EventType::Series
            | EventType::Configuration
            | EventType::Message => None,
        }
    }
}

/// Serde for DXLink's JSONDouble encoding.
///
/// Crate-private: it exists to wire `#[serde(with = ...)]` on the event structs,
/// and exposing it would commit the crate to supporting its surface forever.
///
/// JSON has no literal for a non-finite number, so the protocol sends them as
/// the strings `"NaN"`, `"Infinity"` and `"-Infinity"`. These are ordinary
/// values in market data, not anomalies: an option with no bid has a `NaN`
/// price. Deriving plain `f64` meant such a payload failed to deserialize, and
/// serializing one produced JSON the protocol does not accept.
pub(crate) mod json_double {
    use serde::de::{Error, Unexpected};
    use serde::{Deserialize, Deserializer, Serializer};
    use serde_json::Value;

    /// Emits the protocol's string form for non-finite values.
    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        if value.is_nan() {
            serializer.serialize_str("NaN")
        } else if value.is_infinite() {
            serializer.serialize_str(if value.is_sign_positive() {
                "Infinity"
            } else {
                "-Infinity"
            })
        } else {
            serializer.serialize_f64(*value)
        }
    }

    /// Reads a JSONDouble from a JSON value.
    ///
    /// The one place the number-or-special-string mapping lives. Both this
    /// module, which handles FULL payloads, and the COMPACT decoder in
    /// `utils.rs` go through it, so a future addition to the protocol's set of
    /// specials cannot be applied to one format and forgotten in the other.
    pub(crate) fn from_value(value: &Value) -> Option<f64> {
        if let Some(number) = value.as_f64() {
            return Some(number);
        }
        match value.as_str()? {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => None,
        }
    }

    /// Accepts a JSON number or one of the protocol's non-finite strings.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        let value = Value::deserialize(deserializer)?;

        if let Some(number) = from_value(&value) {
            return Ok(number);
        }

        match value.as_str() {
            Some(other) => Err(D::Error::invalid_value(
                Unexpected::Str(other),
                &"a number, or \"NaN\", \"Infinity\" or \"-Infinity\"",
            )),
            None => Err(D::Error::custom(format!(
                "expected a JSONDouble, got {value}"
            ))),
        }
    }
}

/// Represents a quote event for a financial instrument.
///
/// This structure holds information about a specific quote event, including the type of event,
/// the symbol it relates to, and the bid and ask prices and sizes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteEvent {
    /// The type of the event.  For example, "QUOTE".
    #[serde(rename = "eventType")]
    pub event_type: String,

    /// The symbol the quote relates to. For example, "MSFT".
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,

    /// The bid price for the instrument.
    #[serde(rename = "bidPrice")]
    #[serde(with = "json_double")]
    pub bid_price: f64,

    /// The ask price for the instrument.
    #[serde(rename = "askPrice")]
    #[serde(with = "json_double")]
    pub ask_price: f64,

    /// The size of the bid.
    #[serde(rename = "bidSize")]
    #[serde(with = "json_double")]
    pub bid_size: f64,

    /// The size of the ask.
    #[serde(rename = "askSize")]
    #[serde(with = "json_double")]
    pub ask_size: f64,
}

/// Represents a trade event with details like event type, symbol, price, size, and day volume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeEvent {
    /// The type of the event (e.g., "trade").
    #[serde(rename = "eventType")]
    pub event_type: String,
    /// The symbol of the traded asset (e.g., "BTCUSD").
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,
    /// The price of the trade.
    #[serde(rename = "price")]
    #[serde(with = "json_double")]
    pub price: f64,
    /// The size or quantity of the trade.
    #[serde(rename = "size")]
    #[serde(with = "json_double")]
    pub size: f64,
    /// The total trading volume for the day.
    #[serde(rename = "dayVolume")]
    #[serde(with = "json_double")]
    pub day_volume: f64,
}

/// Represents Greek values for a specific event.  Provides data for various risk measures
/// related to option pricing.  Serializes and deserializes to JSON using `serde`.
///
/// # Examples
///
/// ```
/// use serde::{Serialize, Deserialize};
/// use dxlink::events::GreeksEvent;
///
/// let greeks_event = GreeksEvent {
///     event_type: "example_type".to_string(),
///     event_symbol: "example_symbol".to_string(),
///     delta: 0.5,
///     gamma: 0.2,
///     theta: -0.1,
///     vega: 0.8,
///     rho: 0.05,
///     volatility: 0.25,
/// };
///
/// let json_string = serde_json::to_string(&greeks_event).unwrap();
/// println!("{}", json_string);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GreeksEvent {
    /// The type of the event.  This field is serialized as `eventType`.
    #[serde(rename = "eventType")]
    pub event_type: String,

    /// The symbol associated with the event. This field is serialized as `eventSymbol`.
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,

    /// The delta value. This field is serialized as `delta`.
    #[serde(rename = "delta")]
    #[serde(with = "json_double")]
    pub delta: f64,

    /// The gamma value. This field is serialized as `gamma`.
    #[serde(rename = "gamma")]
    #[serde(with = "json_double")]
    pub gamma: f64,

    /// The theta value. This field is serialized as `theta`.
    #[serde(rename = "theta")]
    #[serde(with = "json_double")]
    pub theta: f64,

    /// The vega value. This field is serialized as `vega`.
    #[serde(rename = "vega")]
    #[serde(with = "json_double")]
    pub vega: f64,

    /// The rho value. This field is serialized as `rho`.
    #[serde(rename = "rho")]
    #[serde(with = "json_double")]
    pub rho: f64,

    /// The volatility value. This field is serialized as `volatility`.
    #[serde(rename = "volatility")]
    #[serde(with = "json_double")]
    pub volatility: f64,
}

/// One OHLC bar for a period, the shape historical data comes back in.
///
/// Candle symbols carry the period, for example `AAPL{=5m}` for five minute
/// bars. Field names and their meaning follow the dxFeed AsyncAPI schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandleEvent {
    /// The type of the event, `Candle`.
    #[serde(rename = "eventType")]
    pub event_type: String,

    /// The candle symbol, including its period, such as `AAPL{=5m}`.
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,

    /// Start of the bar, as epoch milliseconds.
    #[serde(rename = "time")]
    pub time: i64,

    /// First price in the bar.
    #[serde(rename = "open", with = "json_double")]
    pub open: f64,

    /// Highest price in the bar.
    #[serde(rename = "high", with = "json_double")]
    pub high: f64,

    /// Lowest price in the bar.
    #[serde(rename = "low", with = "json_double")]
    pub low: f64,

    /// Last price in the bar.
    #[serde(rename = "close", with = "json_double")]
    pub close: f64,

    /// Total volume traded during the bar.
    #[serde(rename = "volume", with = "json_double")]
    pub volume: f64,
}

/// Represents a market event, which can be a quote, trade, or greeks event.
///
/// This enum uses `serde`'s untagged enum serialization, meaning that the serialized
/// representation will be the same as the serialized representation of the contained
/// variant.  This allows for flexible handling of different event types in a
/// single stream or data structure.
///
/// # Examples
///
/// ```
/// use serde::{Serialize, Deserialize};
/// use dxlink::events::{GreeksEvent, QuoteEvent, TradeEvent};
/// use dxlink::MarketEvent;
///
/// // Create a QuoteEvent
/// let quote_event = MarketEvent::Quote(QuoteEvent {
///     event_type: "QUOTE".to_string(),
///     event_symbol: "MSFT".to_string(),
///     bid_price: 150.00,
///     ask_price: 150.05,
///     bid_size: 1000.0,
///     ask_size: 500.0,
/// });
///
/// // Create a TradeEvent
/// let trade_event = MarketEvent::Trade(TradeEvent {
///     event_type: "TRADE".to_string(),
///     event_symbol: "AAPL".to_string(),
///     price: 175.50,
///     size: 100.0,
///     day_volume: 1000000.0,
/// });
///
/// // Create a GreeksEvent
/// let greeks_event = MarketEvent::Greeks(GreeksEvent {
///     event_type: "GREEKS".to_string(),
///     event_symbol: "TSLA".to_string(),
///     delta: 0.5,
///     gamma: 0.2,
///     theta: -0.1,
///     vega: 0.8,
///     rho: 0.05,
///     volatility: 0.25,
/// });
///
/// // Serialize the events to JSON
/// let quote_json = serde_json::to_string(&quote_event).unwrap();
/// let trade_json = serde_json::to_string(&trade_event).unwrap();
/// let greeks_json = serde_json::to_string(&greeks_event).unwrap();
///
/// println!("Quote Event JSON: {}", quote_json);
/// println!("Trade Event JSON: {}", trade_json);
/// println!("Greeks Event JSON: {}", greeks_json);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MarketEvent {
    /// Represents a Quote event. This enum variant holds a `QuoteEvent` struct,
    /// which contains details about a specific quote event, including the type of event,
    /// the symbol it relates to, and the bid and ask prices and sizes.
    Quote(QuoteEvent),
    /// Represents a Trade event. This is typically a market trade that has occurred.
    Trade(TradeEvent),
    /// Represents a Greeks event, containing Greek values (delta, gamma, theta, vega, rho)
    /// for a specific financial instrument.
    Greeks(GreeksEvent),
    /// One OHLC bar, from a historical or streaming candle subscription.
    ///
    /// Last in the list on purpose: `MarketEvent` is `#[serde(untagged)]`, so
    /// serde tries variants in declaration order and keeps the first that
    /// deserializes. A candle has fields none of the others do, but ordering it
    /// after them keeps the existing three matching exactly as before.
    Candle(CandleEvent),
}

/// Represents compact data, which can be either an event type (string) or a vector of JSON values.
///
/// This enum uses `serde`'s `untagged` attribute, allowing it to serialize and deserialize
/// without an explicit tag.  This means the serialized representation will be either a string
/// (for `EventType`) or an array (for `Values`).
///
/// # Examples
///
/// ```rust
/// use serde_json::{json, Value};
/// use dxlink::events::CompactData;
///
/// let event_type = CompactData::EventType("page_load".to_string());
/// let serialized_event_type = serde_json::to_string(&event_type).unwrap();
/// assert_eq!(serialized_event_type, "\"page_load\"");
///
/// let values = CompactData::Values(vec![json!(1), json!("hello")]);
/// let serialized_values = serde_json::to_string(&values).unwrap();
/// assert_eq!(serialized_values, "[1,\"hello\"]");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompactData {
    /// Represents the type of event.  Currently, only "message" is supported.
    EventType(String),
    /// Represents a collection of JSON values.  This can be used to hold an array
    Values(Vec<serde_json::Value>),
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, from_str, json, to_string};

    #[test]
    fn test_event_type_display() {
        assert_eq!(EventType::Quote.to_string(), "Quote");
        assert_eq!(EventType::Trade.to_string(), "Trade");
        assert_eq!(EventType::Summary.to_string(), "Summary");
        assert_eq!(EventType::Profile.to_string(), "Profile");
        assert_eq!(EventType::Order.to_string(), "Order");
        assert_eq!(EventType::TimeAndSale.to_string(), "TimeAndSale");
        assert_eq!(EventType::Candle.to_string(), "Candle");
        assert_eq!(EventType::TradeETH.to_string(), "TradeETH");
        assert_eq!(EventType::SpreadOrder.to_string(), "SpreadOrder");
        assert_eq!(EventType::Greeks.to_string(), "Greeks");
        assert_eq!(EventType::TheoPrice.to_string(), "TheoPrice");
        assert_eq!(EventType::Underlying.to_string(), "Underlying");
        assert_eq!(EventType::Series.to_string(), "Series");
        assert_eq!(EventType::Configuration.to_string(), "Configuration");
        assert_eq!(EventType::Message.to_string(), "Message");
    }

    #[test]
    fn test_event_type_from_str() {
        assert_eq!(EventType::from("Quote"), EventType::Quote);
        assert_eq!(EventType::from("Trade"), EventType::Trade);
        assert_eq!(EventType::from("Summary"), EventType::Summary);
        assert_eq!(EventType::from("Profile"), EventType::Profile);
        assert_eq!(EventType::from("Order"), EventType::Order);
        assert_eq!(EventType::from("TimeAndSale"), EventType::TimeAndSale);
        assert_eq!(EventType::from("Candle"), EventType::Candle);
        assert_eq!(EventType::from("TradeETH"), EventType::TradeETH);
        assert_eq!(EventType::from("SpreadOrder"), EventType::SpreadOrder);
        assert_eq!(EventType::from("Greeks"), EventType::Greeks);
        assert_eq!(EventType::from("TheoPrice"), EventType::TheoPrice);
        assert_eq!(EventType::from("Underlying"), EventType::Underlying);
        assert_eq!(EventType::from("Series"), EventType::Series);
        assert_eq!(EventType::from("Configuration"), EventType::Configuration);
        assert_eq!(EventType::from("Message"), EventType::Message);

        assert_eq!(EventType::from("UnknownType"), EventType::Quote);
        assert_eq!(EventType::from(""), EventType::Quote);
    }

    #[test]
    fn test_event_type_serialization() {
        let event_type = EventType::Quote;
        let serialized = to_string(&event_type).unwrap();
        assert_eq!(serialized, "\"Quote\"");

        let event_type = EventType::Greeks;
        let serialized = to_string(&event_type).unwrap();
        assert_eq!(serialized, "\"Greeks\"");
    }

    #[test]
    fn test_event_type_deserialization() {
        let event_type: EventType = from_str("\"Quote\"").unwrap();
        assert_eq!(event_type, EventType::Quote);

        let event_type: EventType = from_str("\"Greeks\"").unwrap();
        assert_eq!(event_type, EventType::Greeks);
    }

    #[test]
    fn test_quote_event_serialization() {
        let quote = QuoteEvent {
            event_type: "Quote".to_string(),
            event_symbol: "AAPL".to_string(),
            bid_price: 150.25,
            ask_price: 150.50,
            bid_size: 100.0,
            ask_size: 150.0,
        };

        let serialized = to_string(&quote).unwrap();
        let json_value: Value = from_str(&serialized).unwrap();

        assert_eq!(json_value["eventType"], "Quote");
        assert_eq!(json_value["eventSymbol"], "AAPL");
        assert_eq!(json_value["bidPrice"], 150.25);
        assert_eq!(json_value["askPrice"], 150.50);
        assert_eq!(json_value["bidSize"], 100.0);
        assert_eq!(json_value["askSize"], 150.0);
    }

    #[test]
    fn test_quote_event_deserialization() {
        let json_str = r#"{
            "eventType": "Quote",
            "eventSymbol": "AAPL",
            "bidPrice": 150.25,
            "askPrice": 150.50,
            "bidSize": 100.0,
            "askSize": 150.0
        }"#;

        let quote: QuoteEvent = from_str(json_str).unwrap();

        assert_eq!(quote.event_type, "Quote");
        assert_eq!(quote.event_symbol, "AAPL");
        assert_eq!(quote.bid_price, 150.25);
        assert_eq!(quote.ask_price, 150.50);
        assert_eq!(quote.bid_size, 100.0);
        assert_eq!(quote.ask_size, 150.0);
    }

    #[test]
    fn test_trade_event_serialization() {
        let trade = TradeEvent {
            event_type: "Trade".to_string(),
            event_symbol: "MSFT".to_string(),
            price: 280.75,
            size: 50.0,
            day_volume: 5000000.0,
        };

        let serialized = to_string(&trade).unwrap();
        let json_value: Value = from_str(&serialized).unwrap();

        assert_eq!(json_value["eventType"], "Trade");
        assert_eq!(json_value["eventSymbol"], "MSFT");
        assert_eq!(json_value["price"], 280.75);
        assert_eq!(json_value["size"], 50.0);
        assert_eq!(json_value["dayVolume"], 5000000.0);
    }

    #[test]
    fn test_trade_event_deserialization() {
        let json_str = r#"{
            "eventType": "Trade",
            "eventSymbol": "MSFT",
            "price": 280.75,
            "size": 50.0,
            "dayVolume": 5000000.0
        }"#;

        let trade: TradeEvent = from_str(json_str).unwrap();

        assert_eq!(trade.event_type, "Trade");
        assert_eq!(trade.event_symbol, "MSFT");
        assert_eq!(trade.price, 280.75);
        assert_eq!(trade.size, 50.0);
        assert_eq!(trade.day_volume, 5000000.0);
    }

    #[test]
    fn test_greeks_event_serialization() {
        let greeks = GreeksEvent {
            event_type: "Greeks".to_string(),
            event_symbol: "AAPL230519C00160000".to_string(),
            delta: 0.65,
            gamma: 0.05,
            theta: -0.15,
            vega: 0.10,
            rho: 0.03,
            volatility: 0.25,
        };

        let serialized = to_string(&greeks).unwrap();

        let json_value: Value = from_str(&serialized).unwrap();

        assert_eq!(json_value["eventType"], "Greeks");
        assert_eq!(json_value["eventSymbol"], "AAPL230519C00160000");
        assert_eq!(json_value["delta"], 0.65);
        assert_eq!(json_value["gamma"], 0.05);
        assert_eq!(json_value["theta"], -0.15);
        assert_eq!(json_value["vega"], 0.10);
        assert_eq!(json_value["rho"], 0.03);
        assert_eq!(json_value["volatility"], 0.25);
    }

    #[test]
    fn test_greeks_event_deserialization() {
        let json_str = r#"{
            "eventType": "Greeks",
            "eventSymbol": "AAPL230519C00160000",
            "delta": 0.65,
            "gamma": 0.05,
            "theta": -0.15,
            "vega": 0.10,
            "rho": 0.03,
            "volatility": 0.25
        }"#;

        let greeks: GreeksEvent = from_str(json_str).unwrap();

        assert_eq!(greeks.event_type, "Greeks");
        assert_eq!(greeks.event_symbol, "AAPL230519C00160000");
        assert_eq!(greeks.delta, 0.65);
        assert_eq!(greeks.gamma, 0.05);
        assert_eq!(greeks.theta, -0.15);
        assert_eq!(greeks.vega, 0.10);
        assert_eq!(greeks.rho, 0.03);
        assert_eq!(greeks.volatility, 0.25);
    }

    #[test]
    fn test_market_event_quote_serialization() {
        let quote = QuoteEvent {
            event_type: "Quote".to_string(),
            event_symbol: "AAPL".to_string(),
            bid_price: 150.25,
            ask_price: 150.50,
            bid_size: 100.0,
            ask_size: 150.0,
        };
        let market_event = MarketEvent::Quote(quote);
        let serialized = to_string(&market_event).unwrap();
        let json_value: Value = from_str(&serialized).unwrap();

        assert_eq!(json_value["eventType"], "Quote");
        assert_eq!(json_value["eventSymbol"], "AAPL");
        assert_eq!(json_value["bidPrice"], 150.25);
        assert_eq!(json_value["askPrice"], 150.50);
        assert_eq!(json_value["bidSize"], 100.0);
        assert_eq!(json_value["askSize"], 150.0);
    }

    #[test]
    fn test_market_event_trade_serialization() {
        let trade = TradeEvent {
            event_type: "Trade".to_string(),
            event_symbol: "MSFT".to_string(),
            price: 280.75,
            size: 50.0,
            day_volume: 5000000.0,
        };
        let market_event = MarketEvent::Trade(trade);
        let serialized = to_string(&market_event).unwrap();
        let json_value: Value = from_str(&serialized).unwrap();

        assert_eq!(json_value["eventType"], "Trade");
        assert_eq!(json_value["eventSymbol"], "MSFT");
        assert_eq!(json_value["price"], 280.75);
        assert_eq!(json_value["size"], 50.0);
        assert_eq!(json_value["dayVolume"], 5000000.0);
    }

    #[test]
    fn test_market_event_greeks_serialization() {
        let greeks = GreeksEvent {
            event_type: "Greeks".to_string(),
            event_symbol: "AAPL230519C00160000".to_string(),
            delta: 0.65,
            gamma: 0.05,
            theta: -0.15,
            vega: 0.10,
            rho: 0.03,
            volatility: 0.25,
        };
        let market_event = MarketEvent::Greeks(greeks);
        let serialized = to_string(&market_event).unwrap();
        let json_value: Value = from_str(&serialized).unwrap();

        assert_eq!(json_value["eventType"], "Greeks");
        assert_eq!(json_value["eventSymbol"], "AAPL230519C00160000");
        assert_eq!(json_value["delta"], 0.65);
        assert_eq!(json_value["gamma"], 0.05);
        assert_eq!(json_value["theta"], -0.15);
        assert_eq!(json_value["vega"], 0.10);
        assert_eq!(json_value["rho"], 0.03);
        assert_eq!(json_value["volatility"], 0.25);
    }

    #[test]
    fn test_market_event_quote_deserialization() {
        let json_str = r#"{
            "eventType": "Quote",
            "eventSymbol": "AAPL",
            "bidPrice": 150.25,
            "askPrice": 150.50,
            "bidSize": 100.0,
            "askSize": 150.0
        }"#;

        let market_event: MarketEvent = from_str(json_str).unwrap();
        match market_event {
            MarketEvent::Quote(quote) => {
                assert_eq!(quote.event_type, "Quote");
                assert_eq!(quote.event_symbol, "AAPL");
                assert_eq!(quote.bid_price, 150.25);
                assert_eq!(quote.ask_price, 150.50);
                assert_eq!(quote.bid_size, 100.0);
                assert_eq!(quote.ask_size, 150.0);
            }
            _ => panic!("Expected QuoteEvent"),
        }
    }

    #[test]
    fn test_market_event_trade_deserialization() {
        let json_str = r#"{
            "eventType": "Trade",
            "eventSymbol": "MSFT",
            "price": 280.75,
            "size": 50.0,
            "dayVolume": 5000000.0
        }"#;

        let market_event: MarketEvent = from_str(json_str).unwrap();
        match market_event {
            MarketEvent::Trade(trade) => {
                assert_eq!(trade.event_type, "Trade");
                assert_eq!(trade.event_symbol, "MSFT");
                assert_eq!(trade.price, 280.75);
                assert_eq!(trade.size, 50.0);
                assert_eq!(trade.day_volume, 5000000.0);
            }
            _ => panic!("Expected TradeEvent"),
        }
    }

    #[test]
    fn test_market_event_greeks_deserialization() {
        let json_str = r#"{
            "eventType": "Greeks",
            "eventSymbol": "AAPL230519C00160000",
            "delta": 0.65,
            "gamma": 0.05,
            "theta": -0.15,
            "vega": 0.10,
            "rho": 0.03,
            "volatility": 0.25
        }"#;

        let market_event: MarketEvent = from_str(json_str).unwrap();
        match market_event {
            MarketEvent::Greeks(greeks) => {
                assert_eq!(greeks.event_type, "Greeks");
                assert_eq!(greeks.event_symbol, "AAPL230519C00160000");
                assert_eq!(greeks.delta, 0.65);
                assert_eq!(greeks.gamma, 0.05);
                assert_eq!(greeks.theta, -0.15);
                assert_eq!(greeks.vega, 0.10);
                assert_eq!(greeks.rho, 0.03);
                assert_eq!(greeks.volatility, 0.25);
            }
            _ => panic!("Expected GreeksEvent"),
        }
    }

    #[test]
    fn test_compact_data_eventtype_serialization() {
        let compact_data = CompactData::EventType("Quote".to_string());
        let serialized = to_string(&compact_data).unwrap();
        assert_eq!(serialized, "\"Quote\"");
    }

    #[test]
    fn test_compact_data_values_serialization() {
        let values = vec![
            json!("AAPL"),
            json!("Quote"),
            json!(150.25),
            json!(150.50),
            json!(100.0),
            json!(150.0),
        ];
        let compact_data = CompactData::Values(values);
        let serialized = to_string(&compact_data).unwrap();
        assert_eq!(serialized, "[\"AAPL\",\"Quote\",150.25,150.5,100.0,150.0]");
    }

    #[test]
    fn test_compact_data_eventtype_deserialization() {
        let json_str = "\"Quote\"";
        let compact_data: CompactData = from_str(json_str).unwrap();
        match compact_data {
            CompactData::EventType(event_type) => {
                assert_eq!(event_type, "Quote");
            }
            _ => panic!("Expected CompactData::EventType"),
        }
    }

    #[test]
    fn test_compact_data_values_deserialization() {
        let json_str = "[\"AAPL\",\"Quote\",150.25,150.5,100.0,150.0]";
        let compact_data: CompactData = from_str(json_str).unwrap();
        match compact_data {
            CompactData::Values(values) => {
                assert_eq!(values.len(), 6);
                assert_eq!(values[0], json!("AAPL"));
                assert_eq!(values[1], json!("Quote"));
                assert_eq!(values[2], json!(150.25));
                assert_eq!(values[3], json!(150.5));
                assert_eq!(values[4], json!(100.0));
                assert_eq!(values[5], json!(150.0));
            }
            _ => panic!("Expected CompactData::Values"),
        }
    }
}

#[cfg(test)]
mod wire_name_tests {
    use super::*;

    /// Every declared type must survive Display then FromStr.
    ///
    /// This is what a maintainer adding an `EventType` variant has to update:
    /// `from_wire_name` matches on `&str`, so a new variant compiles without it
    /// and then fails this round trip.
    #[test]
    fn test_every_event_type_round_trips_through_its_wire_name() {
        for event_type in ALL_EVENT_TYPES {
            let name = event_type.to_string();
            assert_eq!(
                EventType::from_wire_name(&name),
                Some(event_type),
                "`{name}` does not parse back; from_wire_name is missing an arm"
            );
        }
    }

    /// The array itself must not silently fall behind the enum.
    #[test]
    fn test_all_event_types_covers_every_declared_name() {
        // Each entry is distinct, so a copy-paste omission shows up as a
        // duplicate or a short list rather than passing quietly.
        let mut names: Vec<String> = ALL_EVENT_TYPES.iter().map(|e| e.to_string()).collect();
        names.sort();
        names.dedup();
        assert_eq!(
            names.len(),
            ALL_EVENT_TYPES.len(),
            "ALL_EVENT_TYPES contains a duplicate"
        );
    }
}

#[cfg(test)]
mod json_double_tests {
    use super::*;
    use serde_json::json;

    /// An option with no bid has a NaN price. The protocol sends it as a string
    /// because JSON has no literal, and deriving plain f64 rejected it.
    #[test]
    fn test_a_full_payload_with_non_finite_values_deserializes() {
        let quote: QuoteEvent = serde_json::from_value(json!({
            "eventType": "Quote",
            "eventSymbol": "AAPL240119C00500000",
            "bidPrice": "NaN",
            "askPrice": "Infinity",
            "bidSize": "-Infinity",
            "askSize": 150.0
        }))
        .expect("the protocol's non-finite strings are valid");

        assert!(quote.bid_price.is_nan());
        assert_eq!(quote.ask_price, f64::INFINITY);
        assert_eq!(quote.bid_size, f64::NEG_INFINITY);
        assert_eq!(quote.ask_size, 150.0);
    }

    /// Serializing must produce what the protocol accepts, not JSON `null` or a
    /// serializer error.
    #[test]
    fn test_non_finite_values_serialize_to_the_protocol_strings() {
        let json = serde_json::to_value(QuoteEvent {
            event_type: "Quote".to_string(),
            event_symbol: "AAPL".to_string(),
            bid_price: f64::NAN,
            ask_price: f64::INFINITY,
            bid_size: f64::NEG_INFINITY,
            ask_size: 150.0,
        })
        .expect("failed to serialize");

        assert_eq!(json["bidPrice"], "NaN");
        assert_eq!(json["askPrice"], "Infinity");
        assert_eq!(json["bidSize"], "-Infinity");
        // Finite values stay numbers; quoting them all would be a wire change.
        assert_eq!(json["askSize"], 150.0);
    }

    #[test]
    fn test_finite_values_round_trip_exactly() {
        let original = TradeEvent {
            event_type: "Trade".to_string(),
            event_symbol: "MSFT".to_string(),
            price: 280.75,
            size: 50.0,
            day_volume: 5_000_000.0,
        };

        let back: TradeEvent =
            serde_json::from_str(&serde_json::to_string(&original).expect("serialize"))
                .expect("deserialize");

        assert_eq!(back.price, 280.75);
        assert_eq!(back.size, 50.0);
        assert_eq!(back.day_volume, 5_000_000.0);
    }

    #[test]
    fn test_non_finite_values_round_trip_through_full_json() {
        let original = GreeksEvent {
            event_type: "Greeks".to_string(),
            event_symbol: "AAPL240119C00500000".to_string(),
            delta: f64::NAN,
            gamma: 0.05,
            theta: f64::NEG_INFINITY,
            vega: 0.1,
            rho: f64::INFINITY,
            volatility: 0.25,
        };

        let back: GreeksEvent =
            serde_json::from_str(&serde_json::to_string(&original).expect("serialize"))
                .expect("deserialize");

        assert!(back.delta.is_nan());
        assert_eq!(back.theta, f64::NEG_INFINITY);
        assert_eq!(back.rho, f64::INFINITY);
        assert_eq!(back.gamma, 0.05);
    }

    #[test]
    fn test_a_string_that_is_not_a_known_special_is_rejected() {
        let result: Result<QuoteEvent, _> = serde_json::from_value(json!({
            "eventType": "Quote",
            "eventSymbol": "AAPL",
            "bidPrice": "cheap",
            "askPrice": 1.0,
            "bidSize": 1.0,
            "askSize": 1.0
        }));

        let error = result.expect_err("`cheap` is not a JSONDouble").to_string();
        assert!(error.contains("cheap"), "the value is missing: {error}");
    }
}
