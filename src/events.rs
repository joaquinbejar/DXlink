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

// Eventos específicos
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

// Manejo de formato COMPACT
pub fn parse_compact_data(data: &[CompactData]) -> Vec<MarketEvent> {
    let mut events = Vec::new();
    let mut i = 0;

    while i < data.len() {
        if let CompactData::EventType(event_type) = &data[i] {
            i += 1;
            if i < data.len() {
                if let CompactData::Values(values) = &data[i] {
                    match event_type.as_str() {
                        "Quote" => {
                            let mut j = 0;
                            while j + 5 < values.len() {
                                if let (
                                    Some(symbol),
                                    Some(_), // event_type
                                    Some(bid_price),
                                    Some(ask_price),
                                    Some(bid_size),
                                    Some(ask_size),
                                ) = (
                                    values.get(j).and_then(|v| v.as_str()),
                                    values.get(j + 1).and_then(|v| v.as_str()),
                                    values
                                        .get(j + 2)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 3)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 4)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 5)
                                        .and_then(|v| v.as_f64()),
                                ) {
                                    events.push(MarketEvent::Quote(QuoteEvent {
                                        event_type: "Quote".to_string(),
                                        event_symbol: symbol.to_string(),
                                        bid_price,
                                        ask_price,
                                        bid_size,
                                        ask_size,
                                    }));
                                }
                                j += 6;
                            }
                        }
                        "Trade" => {
                            // Parsear valores de Trade
                            let mut j = 0;
                            while j + 4 < values.len() {
                                if let (
                                    Some(symbol),
                                    Some(_), // event_type
                                    Some(price),
                                    Some(size),
                                    Some(day_volume),
                                ) = (
                                    values.get(j).and_then(|v| v.as_str()),
                                    values.get(j + 1).and_then(|v| v.as_str()),
                                    values
                                        .get(j + 2)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 3)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 4)
                                        .and_then(|v| v.as_f64()),
                                ) {
                                    events.push(MarketEvent::Trade(TradeEvent {
                                        event_type: "Trade".to_string(),
                                        event_symbol: symbol.to_string(),
                                        price,
                                        size,
                                        day_volume,
                                    }));
                                }
                                j += 5;
                            }
                        }
                        "Greeks" => {
                            // Parsear valores de Greeks (simplificado)
                            let mut j = 0;
                            while j + 7 < values.len() {
                                if let (
                                    Some(symbol),
                                    Some(_), // event_type
                                    Some(delta),
                                    Some(gamma),
                                    Some(theta),
                                    Some(vega),
                                    Some(rho),
                                    Some(volatility),
                                ) = (
                                    values.get(j).and_then(|v| v.as_str()),
                                    values.get(j + 1).and_then(|v| v.as_str()),
                                    values
                                        .get(j + 2)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 3)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 4)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 5)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 6)
                                        .and_then(|v| v.as_f64()),
                                    values
                                        .get(j + 7)
                                        .and_then(|v| v.as_f64()),
                                ) {
                                    events.push(MarketEvent::Greeks(GreeksEvent {
                                        event_type: "Greeks".to_string(),
                                        event_symbol: symbol.to_string(),
                                        delta,
                                        gamma,
                                        theta,
                                        vega,
                                        rho,
                                        volatility,
                                    }));
                                }
                                j += 8;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        i += 1;
    }

    events
}
