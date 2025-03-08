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
    pub bid_price: f64,

    /// The ask price for the instrument.
    #[serde(rename = "askPrice")]
    pub ask_price: f64,

    /// The size of the bid.
    #[serde(rename = "bidSize")]
    pub bid_size: f64,

    /// The size of the ask.
    #[serde(rename = "askSize")]
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
    pub price: f64,
    /// The size or quantity of the trade.
    #[serde(rename = "size")]
    pub size: f64,
    /// The total trading volume for the day.
    #[serde(rename = "dayVolume")]
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
    pub delta: f64,

    /// The gamma value. This field is serialized as `gamma`.
    #[serde(rename = "gamma")]
    pub gamma: f64,

    /// The theta value. This field is serialized as `theta`.
    #[serde(rename = "theta")]
    pub theta: f64,

    /// The vega value. This field is serialized as `vega`.
    #[serde(rename = "vega")]
    pub vega: f64,

    /// The rho value. This field is serialized as `rho`.
    #[serde(rename = "rho")]
    pub rho: f64,

    /// The volatility value. This field is serialized as `volatility`.
    #[serde(rename = "volatility")]
    pub volatility: f64,
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
