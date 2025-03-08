/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 8/3/25
******************************************************************************/
use crate::MarketEvent;
use crate::events::{CompactData, GreeksEvent, QuoteEvent, TradeEvent};

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
                                    values.get(j + 2).and_then(|v| v.as_f64()),
                                    values.get(j + 3).and_then(|v| v.as_f64()),
                                    values.get(j + 4).and_then(|v| v.as_f64()),
                                    values.get(j + 5).and_then(|v| v.as_f64()),
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
                                    values.get(j + 2).and_then(|v| v.as_f64()),
                                    values.get(j + 3).and_then(|v| v.as_f64()),
                                    values.get(j + 4).and_then(|v| v.as_f64()),
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
                                    values.get(j + 2).and_then(|v| v.as_f64()),
                                    values.get(j + 3).and_then(|v| v.as_f64()),
                                    values.get(j + 4).and_then(|v| v.as_f64()),
                                    values.get(j + 5).and_then(|v| v.as_f64()),
                                    values.get(j + 6).and_then(|v| v.as_f64()),
                                    values.get(j + 7).and_then(|v| v.as_f64()),
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


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_compact_data_empty() {
        let data: Vec<CompactData> = vec![];
        let events = parse_compact_data(&data);

        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_compact_data_quote() {
        let data = vec![
            CompactData::EventType("Quote".to_string()),
            CompactData::Values(vec![
                json!("AAPL"),    // symbol
                json!("Quote"),   // event_type
                json!(150.25),    // bid_price
                json!(150.50),    // ask_price
                json!(100.0),     // bid_size
                json!(150.0),     // ask_size
            ]),
        ];

        let events = parse_compact_data(&data);
        assert_eq!(events.len(), 1);

        match &events[0] {
            MarketEvent::Quote(quote) => {
                assert_eq!(quote.event_type, "Quote");
                assert_eq!(quote.event_symbol, "AAPL");
                assert_eq!(quote.bid_price, 150.25);
                assert_eq!(quote.ask_price, 150.50);
                assert_eq!(quote.bid_size, 100.0);
                assert_eq!(quote.ask_size, 150.0);
            },
            _ => panic!("Expected QuoteEvent"),
        }
    }

    #[test]
    fn test_parse_compact_data_multiple_quotes() {
        let data = vec![
            CompactData::EventType("Quote".to_string()),
            CompactData::Values(vec![
                // Primer Quote
                json!("AAPL"),    // symbol
                json!("Quote"),   // event_type
                json!(150.25),    // bid_price
                json!(150.50),    // ask_price
                json!(100.0),     // bid_size
                json!(150.0),     // ask_size

                // Segundo Quote
                json!("MSFT"),    // symbol
                json!("Quote"),   // event_type
                json!(280.75),    // bid_price
                json!(281.00),    // ask_price
                json!(80.0),      // bid_size
                json!(120.0),     // ask_size
            ]),
        ];

        let events = parse_compact_data(&data);
        assert_eq!(events.len(), 2);

        match &events[0] {
            MarketEvent::Quote(quote) => {
                assert_eq!(quote.event_type, "Quote");
                assert_eq!(quote.event_symbol, "AAPL");
                assert_eq!(quote.bid_price, 150.25);
                assert_eq!(quote.ask_price, 150.50);
                assert_eq!(quote.bid_size, 100.0);
                assert_eq!(quote.ask_size, 150.0);
            },
            _ => panic!("Expected QuoteEvent for AAPL"),
        }

        match &events[1] {
            MarketEvent::Quote(quote) => {
                assert_eq!(quote.event_type, "Quote");
                assert_eq!(quote.event_symbol, "MSFT");
                assert_eq!(quote.bid_price, 280.75);
                assert_eq!(quote.ask_price, 281.00);
                assert_eq!(quote.bid_size, 80.0);
                assert_eq!(quote.ask_size, 120.0);
            },
            _ => panic!("Expected QuoteEvent for MSFT"),
        }
    }

    #[test]
    fn test_parse_compact_data_trade() {
        let data = vec![
            CompactData::EventType("Trade".to_string()),
            CompactData::Values(vec![
                json!("MSFT"),    // symbol
                json!("Trade"),   // event_type
                json!(280.75),    // price
                json!(50.0),      // size
                json!(5000000.0), // day_volume
            ]),
        ];

        let events = parse_compact_data(&data);
        assert_eq!(events.len(), 1);

        match &events[0] {
            MarketEvent::Trade(trade) => {
                assert_eq!(trade.event_type, "Trade");
                assert_eq!(trade.event_symbol, "MSFT");
                assert_eq!(trade.price, 280.75);
                assert_eq!(trade.size, 50.0);
                assert_eq!(trade.day_volume, 5000000.0);
            },
            _ => panic!("Expected TradeEvent"),
        }
    }

    #[test]
    fn test_parse_compact_data_multiple_trades() {
        let data = vec![
            CompactData::EventType("Trade".to_string()),
            CompactData::Values(vec![
                // Primer Trade
                json!("MSFT"),    // symbol
                json!("Trade"),   // event_type
                json!(280.75),    // price
                json!(50.0),      // size
                json!(5000000.0), // day_volume

                // Segundo Trade
                json!("AAPL"),    // symbol
                json!("Trade"),   // event_type
                json!(150.25),    // price
                json!(100.0),     // size
                json!(8000000.0), // day_volume
            ]),
        ];

        let events = parse_compact_data(&data);

        assert_eq!(events.len(), 2);

        match &events[0] {
            MarketEvent::Trade(trade) => {
                assert_eq!(trade.event_type, "Trade");
                assert_eq!(trade.event_symbol, "MSFT");
                assert_eq!(trade.price, 280.75);
                assert_eq!(trade.size, 50.0);
                assert_eq!(trade.day_volume, 5000000.0);
            },
            _ => panic!("Expected TradeEvent for MSFT"),
        }

        match &events[1] {
            MarketEvent::Trade(trade) => {
                assert_eq!(trade.event_type, "Trade");
                assert_eq!(trade.event_symbol, "AAPL");
                assert_eq!(trade.price, 150.25);
                assert_eq!(trade.size, 100.0);
                assert_eq!(trade.day_volume, 8000000.0);
            },
            _ => panic!("Expected TradeEvent for AAPL"),
        }
    }

    #[test]
    fn test_parse_compact_data_greeks() {
        // Crear datos compactos para un evento Greeks
        let data = vec![
            CompactData::EventType("Greeks".to_string()),
            CompactData::Values(vec![
                json!("AAPL230519C00160000"),  // symbol
                json!("Greeks"),               // event_type
                json!(0.65),                   // delta
                json!(0.05),                   // gamma
                json!(-0.15),                  // theta
                json!(0.10),                   // vega
                json!(0.03),                   // rho
                json!(0.25),                   // volatility
            ]),
        ];

        let events = parse_compact_data(&data);
        assert_eq!(events.len(), 1);

        match &events[0] {
            MarketEvent::Greeks(greeks) => {
                assert_eq!(greeks.event_type, "Greeks");
                assert_eq!(greeks.event_symbol, "AAPL230519C00160000");
                assert_eq!(greeks.delta, 0.65);
                assert_eq!(greeks.gamma, 0.05);
                assert_eq!(greeks.theta, -0.15);
                assert_eq!(greeks.vega, 0.10);
                assert_eq!(greeks.rho, 0.03);
                assert_eq!(greeks.volatility, 0.25);
            },
            _ => panic!("Expected GreeksEvent"),
        }
    }
}