/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 7/3/25
******************************************************************************/

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    Quote,
    Trade,
    Summary,
    Profile,
    Order,
    TimeAndSale,
    Candle,
    TradeETH,
    SpreadOrder,
    Greeks,
    TheoPrice,
    Underlying,
    Series,
    Configuration,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteEvent {
    #[serde(rename = "eventType")]
    pub event_type: String,
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,
    #[serde(rename = "bidPrice")]
    pub bid_price: f64,
    #[serde(rename = "askPrice")]
    pub ask_price: f64,
    #[serde(rename = "bidSize")]
    pub bid_size: f64,
    #[serde(rename = "askSize")]
    pub ask_size: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeEvent {
    #[serde(rename = "eventType")]
    pub event_type: String,
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,
    #[serde(rename = "price")]
    pub price: f64,
    #[serde(rename = "size")]
    pub size: f64,
    #[serde(rename = "dayVolume")]
    pub day_volume: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GreeksEvent {
    #[serde(rename = "eventType")]
    pub event_type: String,
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,
    #[serde(rename = "delta")]
    pub delta: f64,
    #[serde(rename = "gamma")]
    pub gamma: f64,
    #[serde(rename = "theta")]
    pub theta: f64,
    #[serde(rename = "vega")]
    pub vega: f64,
    #[serde(rename = "rho")]
    pub rho: f64,
    #[serde(rename = "volatility")]
    pub volatility: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MarketEvent {
    Quote(QuoteEvent),
    Trade(TradeEvent),
    Greeks(GreeksEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompactData {
    EventType(String),
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
