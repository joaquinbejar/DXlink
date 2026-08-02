/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 8/3/25
******************************************************************************/
use crate::MarketEvent;
use crate::error::{DXLinkError, DXLinkResult};
use crate::events::{CompactData, EventType, GreeksEvent, QuoteEvent, TradeEvent};
use serde_json::Value;
use tracing::warn;

/// Decodes COMPACT rows, dropping the whole batch if any of it is malformed.
///
/// **Lossy on purpose, and kept for source compatibility.** It stops at the
/// first thing it cannot read, logs a warning, and returns the events decoded
/// before that point — so a consumer cannot tell corrupt protocol data from a
/// quiet market. Use [`try_parse_compact_data`] for anything that needs to know.
///
/// # Example
///
/// ```rust,no_run
/// use serde_json::Value;
/// use dxlink::events::CompactData;
/// use dxlink::{parse_compact_data, MarketEvent};
///
/// let data = vec![
///     CompactData::EventType("Quote".to_string()),
///     CompactData::Values(vec![
///         Value::from("Quote"),
///         Value::from("AAPL"),
///         Value::from(150.25),
///         Value::from(150.35),
///         Value::from(1000.0),
///         Value::from(2000.0),
///     ]),
/// ];
///
/// let events = parse_compact_data(&data);
/// assert_eq!(events.len(), 1);
///
/// if let MarketEvent::Quote(quote) = &events[0] {
///     assert_eq!(quote.event_symbol, "AAPL");
///     assert_eq!(quote.bid_price, 150.25);
/// }
/// ```
pub fn parse_compact_data(data: &[CompactData]) -> Vec<MarketEvent> {
    let (events, error) = decode(data);
    if let Some(error) = error {
        warn!("Dropping malformed COMPACT data: {error}");
    }
    events
}

/// Decodes COMPACT rows, reporting the first thing it cannot decode.
///
/// Prefer this over [`parse_compact_data`], which cannot tell a consumer the
/// difference between "no events" and "the server sent something this client
/// does not understand". Errors name the event type, the row, the field, what
/// was expected and what arrived.
///
/// # Errors
///
/// [`DXLinkError::Protocol`] for an unsupported event type, a row count that is
/// not a whole number of rows, a column of the wrong type, or an inner
/// `eventType` that disagrees with the batch header.
///
/// # Example
///
/// ```rust
/// use dxlink::events::CompactData;
/// use dxlink::try_parse_compact_data;
/// use serde_json::Value;
///
/// let data = vec![
///     CompactData::EventType("Quote".to_string()),
///     CompactData::Values(vec![
///         Value::from("Quote"),
///         Value::from("AAPL"),
///         Value::from(150.25),
///         Value::from(150.35),
///         Value::from(1000.0),
///         Value::from(2000.0),
///     ]),
/// ];
///
/// let events = try_parse_compact_data(&data).expect("well formed");
/// assert_eq!(events.len(), 1);
/// ```
pub fn try_parse_compact_data(data: &[CompactData]) -> DXLinkResult<Vec<MarketEvent>> {
    match decode(data) {
        (events, None) => Ok(events),
        (_, Some(error)) => Err(error),
    }
}

/// Reads one COMPACT column as a DXLink JSONDouble.
///
/// The protocol encodes non-finite doubles as the strings `"NaN"`, `"Infinity"`
/// and `"-Infinity"`, because JSON has no literal for them. Treating those as a
/// wrong column type would reject data the server is entitled to send: an option
/// with no bid has a `NaN` price, which is ordinary rather than exceptional.
fn as_json_double(value: &Value) -> Option<f64> {
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

/// A column that must be text.
fn as_text<'a>(
    values: &'a [Value],
    index: usize,
    event_type: &str,
    row: usize,
    field: &str,
) -> DXLinkResult<&'a str> {
    values[index].as_str().ok_or_else(|| {
        DXLinkError::Protocol(format!(
            "{event_type} row {row}: field `{field}` should be a string, got {}",
            values[index]
        ))
    })
}

/// A column that must be a double.
fn as_double(
    values: &[Value],
    index: usize,
    event_type: &str,
    row: usize,
    field: &str,
) -> DXLinkResult<f64> {
    as_json_double(&values[index]).ok_or_else(|| {
        DXLinkError::Protocol(format!(
            "{event_type} row {row}: field `{field}` should be a number, got {}",
            values[index]
        ))
    })
}

/// Decodes what it can and reports the first problem it hit.
///
/// The layout comes from [`EventType::compact_fields`], the same list
/// `setup_feed` asked the server for, so the stride cannot drift away from the
/// request the way three separate literals could.
fn decode(data: &[CompactData]) -> (Vec<MarketEvent>, Option<DXLinkError>) {
    let mut events = Vec::new();
    let mut index = 0;

    while index < data.len() {
        let CompactData::EventType(header) = &data[index] else {
            return (
                events,
                Some(DXLinkError::Protocol(format!(
                    "expected an event type at position {index}, got a value array"
                ))),
            );
        };
        index += 1;

        let Some(CompactData::Values(values)) = data.get(index) else {
            return (
                events,
                Some(DXLinkError::Protocol(format!(
                    "event type `{header}` at position {} has no values after it",
                    index - 1
                ))),
            );
        };
        index += 1;

        let Some(event_type) = EventType::from_wire_name(header) else {
            return (
                events,
                Some(DXLinkError::Protocol(format!(
                    "unknown event type `{header}` in COMPACT data"
                ))),
            );
        };

        let Some(fields) = event_type.compact_fields() else {
            return (
                events,
                Some(DXLinkError::Protocol(format!(
                    "event type `{header}` has no decoder in this client, so its rows \
                     cannot be read"
                ))),
            );
        };

        let stride = fields.len();
        if values.len() % stride != 0 {
            return (
                events,
                Some(DXLinkError::Protocol(format!(
                    "{header}: {} value(s) is not a whole number of rows of {stride} \
                     field(s); a truncated row would shift every field that follows",
                    values.len()
                ))),
            );
        }

        for (row, chunk) in values.chunks_exact(stride).enumerate() {
            // The batch header and the row's own eventType must agree, or the
            // row belongs to a layout other than the one being applied.
            let inner = match as_text(chunk, 0, header, row, fields[0]) {
                Ok(inner) => inner,
                Err(e) => return (events, Some(e)),
            };
            if inner != header {
                return (
                    events,
                    Some(DXLinkError::Protocol(format!(
                        "{header} row {row}: the row says it is a `{inner}`, so it is not \
                         laid out the way this batch claims"
                    ))),
                );
            }

            match build_event(event_type, chunk, header, row, fields) {
                Ok(event) => events.push(event),
                Err(e) => return (events, Some(e)),
            }
        }
    }

    (events, None)
}

/// Builds one event from a row whose length already matches the layout.
fn build_event(
    event_type: EventType,
    row_values: &[Value],
    header: &str,
    row: usize,
    fields: &[&str],
) -> DXLinkResult<MarketEvent> {
    let symbol = as_text(row_values, 1, header, row, fields[1])?.to_string();
    let number = |index: usize| as_double(row_values, index, header, row, fields[index]);

    Ok(match event_type {
        EventType::Quote => MarketEvent::Quote(QuoteEvent {
            event_type: header.to_string(),
            event_symbol: symbol,
            bid_price: number(2)?,
            ask_price: number(3)?,
            bid_size: number(4)?,
            ask_size: number(5)?,
        }),
        EventType::Trade => MarketEvent::Trade(TradeEvent {
            event_type: header.to_string(),
            event_symbol: symbol,
            price: number(2)?,
            size: number(3)?,
            day_volume: number(4)?,
        }),
        EventType::Greeks => MarketEvent::Greeks(GreeksEvent {
            event_type: header.to_string(),
            event_symbol: symbol,
            delta: number(2)?,
            gamma: number(3)?,
            theta: number(4)?,
            vega: number(5)?,
            rho: number(6)?,
            volatility: number(7)?,
        }),
        // compact_fields returned Some for this type, so a missing arm here is a
        // bug in this match rather than a protocol problem.
        other => {
            return Err(DXLinkError::Protocol(format!(
                "event type `{other}` has a field layout but no decoder arm"
            )));
        }
    })
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
                json!("Quote"), // event_type
                json!("AAPL"),  // symbol
                json!(150.25),  // bid_price
                json!(150.50),  // ask_price
                json!(100.0),   // bid_size
                json!(150.0),   // ask_size
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
            }
            _ => panic!("Expected QuoteEvent"),
        }
    }

    #[test]
    fn test_parse_compact_data_multiple_quotes() {
        let data = vec![
            CompactData::EventType("Quote".to_string()),
            CompactData::Values(vec![
                // Primer Quote
                json!("Quote"), // event_type
                json!("AAPL"),  // symbol
                json!(150.25),  // bid_price
                json!(150.50),  // ask_price
                json!(100.0),   // bid_size
                json!(150.0),   // ask_size
                // Segundo Quote
                json!("Quote"), // event_type
                json!("MSFT"),  // symbol
                json!(280.75),  // bid_price
                json!(281.00),  // ask_price
                json!(80.0),    // bid_size
                json!(120.0),   // ask_size
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
            }
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
            }
            _ => panic!("Expected QuoteEvent for MSFT"),
        }
    }

    #[test]
    fn test_parse_compact_data_trade() {
        let data = vec![
            CompactData::EventType("Trade".to_string()),
            CompactData::Values(vec![
                json!("Trade"),   // event_type
                json!("MSFT"),    // symbol
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
            }
            _ => panic!("Expected TradeEvent"),
        }
    }

    #[test]
    fn test_parse_compact_data_multiple_trades() {
        let data = vec![
            CompactData::EventType("Trade".to_string()),
            CompactData::Values(vec![
                // Primer Trade
                json!("Trade"),   // event_type
                json!("MSFT"),    // symbol
                json!(280.75),    // price
                json!(50.0),      // size
                json!(5000000.0), // day_volume
                // Segundo Trade
                json!("Trade"),   // event_type
                json!("AAPL"),    // symbol
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
            }
            _ => panic!("Expected TradeEvent for MSFT"),
        }

        match &events[1] {
            MarketEvent::Trade(trade) => {
                assert_eq!(trade.event_type, "Trade");
                assert_eq!(trade.event_symbol, "AAPL");
                assert_eq!(trade.price, 150.25);
                assert_eq!(trade.size, 100.0);
                assert_eq!(trade.day_volume, 8000000.0);
            }
            _ => panic!("Expected TradeEvent for AAPL"),
        }
    }

    #[test]
    fn test_parse_compact_data_greeks() {
        // Crear datos compactos para un evento Greeks
        let data = vec![
            CompactData::EventType("Greeks".to_string()),
            CompactData::Values(vec![
                json!("Greeks"),              // event_type
                json!("AAPL230519C00160000"), // symbol
                json!(0.65),                  // delta
                json!(0.05),                  // gamma
                json!(-0.15),                 // theta
                json!(0.10),                  // vega
                json!(0.03),                  // rho
                json!(0.25),                  // volatility
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
            }
            _ => panic!("Expected GreeksEvent"),
        }
    }
}

#[cfg(test)]
mod strict_tests {
    use super::*;
    use serde_json::json;

    fn quote_row(symbol: &str) -> Vec<Value> {
        vec![
            json!("Quote"),
            json!(symbol),
            json!(150.25),
            json!(150.5),
            json!(100.0),
            json!(150.0),
        ]
    }

    fn batch(header: &str, values: Vec<Value>) -> Vec<CompactData> {
        vec![
            CompactData::EventType(header.to_string()),
            CompactData::Values(values),
        ]
    }

    #[test]
    fn test_two_rows_decode_with_the_right_stride() {
        let mut values = quote_row("AAPL");
        values.extend(quote_row("MSFT"));

        let events = try_parse_compact_data(&batch("Quote", values)).expect("well formed");

        assert_eq!(events.len(), 2);
        match (&events[0], &events[1]) {
            (MarketEvent::Quote(first), MarketEvent::Quote(second)) => {
                // The symbols prove the stride: off by one and the second row
                // reads a price as its symbol.
                assert_eq!(first.event_symbol, "AAPL");
                assert_eq!(second.event_symbol, "MSFT");
                assert_eq!(second.bid_price, 150.25);
            }
            other => panic!("expected two quotes, got {other:?}"),
        }
    }

    #[test]
    fn test_a_truncated_row_is_rejected_not_skipped() {
        let mut values = quote_row("AAPL");
        values.pop();

        let error = try_parse_compact_data(&batch("Quote", values))
            .expect_err("a short row must not decode");
        let text = error.to_string();
        assert!(text.contains("whole number of rows"), "unclear: {text}");
    }

    #[test]
    fn test_trailing_values_are_rejected() {
        let mut values = quote_row("AAPL");
        values.push(json!(1.0));

        assert!(
            try_parse_compact_data(&batch("Quote", values)).is_err(),
            "an extra value shifts every row after it and must not pass"
        );
    }

    #[test]
    fn test_a_wrong_column_type_names_the_field() {
        let mut values = quote_row("AAPL");
        values[2] = json!("not a price");

        let error =
            try_parse_compact_data(&batch("Quote", values)).expect_err("a text price is invalid");
        let text = error.to_string();
        assert!(text.contains("bidPrice"), "the field is missing: {text}");
        assert!(text.contains("row 0"), "the row is missing: {text}");
        assert!(text.contains("not a price"), "the value is missing: {text}");
    }

    #[test]
    fn test_a_row_disagreeing_with_its_header_is_rejected() {
        let mut values = quote_row("AAPL");
        values[0] = json!("Trade");

        let error = try_parse_compact_data(&batch("Quote", values))
            .expect_err("the row is not laid out as the header claims");
        assert!(error.to_string().contains("Trade"), "{error}");
    }

    #[test]
    fn test_an_undecodable_event_type_is_reported() {
        // Declared by the protocol, no decoder here.
        let error = try_parse_compact_data(&batch("Candle", vec![json!("Candle"), json!("AAPL")]))
            .expect_err("Candle has no decoder");
        assert!(error.to_string().contains("no decoder"), "{error}");
    }

    #[test]
    fn test_an_unknown_event_type_is_reported() {
        let error = try_parse_compact_data(&batch("Nonsense", vec![json!("Nonsense")]))
            .expect_err("an unknown name must not be silently treated as Quote");
        assert!(error.to_string().contains("Nonsense"), "{error}");
    }

    #[test]
    fn test_json_double_specials_are_values_not_errors() {
        // An option with no bid has a NaN price. The protocol sends it as a
        // string because JSON has no literal for it.
        let mut values = quote_row("AAPL");
        values[2] = json!("NaN");
        values[3] = json!("Infinity");
        values[4] = json!("-Infinity");

        let events = try_parse_compact_data(&batch("Quote", values)).expect("specials are valid");
        match &events[0] {
            MarketEvent::Quote(quote) => {
                assert!(quote.bid_price.is_nan());
                assert_eq!(quote.ask_price, f64::INFINITY);
                assert_eq!(quote.bid_size, f64::NEG_INFINITY);
            }
            other => panic!("expected a quote, got {other:?}"),
        }
    }

    #[test]
    fn test_a_header_with_no_values_is_reported() {
        let data = vec![CompactData::EventType("Quote".to_string())];
        assert!(try_parse_compact_data(&data).is_err());
    }

    #[test]
    fn test_the_lossy_wrapper_keeps_what_it_could_decode() {
        let mut values = quote_row("AAPL");
        values.extend(quote_row("MSFT"));
        values.pop();

        // Strict rejects the batch; the lossy wrapper is documented to return
        // only what it managed, which is nothing here because the row count is
        // what is wrong.
        assert!(try_parse_compact_data(&batch("Quote", values.clone())).is_err());
        assert!(parse_compact_data(&batch("Quote", values)).is_empty());
    }
}
