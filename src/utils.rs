/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 8/3/25
******************************************************************************/
use crate::MarketEvent;
use crate::error::{DXLinkError, DXLinkResult};
use crate::events::{
    CandleEvent, CompactData, EventType, GreeksEvent, ProfileEvent, QuoteEvent, SeriesEvent,
    SummaryEvent, TheoPriceEvent, TimeAndSaleEvent, TradeETHEvent, TradeEvent, UnderlyingEvent,
};
use serde_json::Value;
use std::collections::HashMap;
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
    let (events, error) = decode(data, None);
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
    match decode(data, None) {
        (events, None) => Ok(events),
        (_, Some(error)) => Err(error),
    }
}

/// Reads one COMPACT column as a DXLink JSONDouble.
///
/// Delegates to the shared mapping in `events::json_double`, so COMPACT and
/// FULL cannot drift apart if the protocol adds another special value.
fn as_json_double(value: &Value) -> Option<f64> {
    crate::events::json_double::from_value(value)
}

/// Decodes against the layout a channel actually negotiated.
///
/// The client uses this rather than [`try_parse_compact_data`], because the
/// list the server agreed to is not always the list that was requested.
pub(crate) fn try_parse_negotiated(
    data: &[CompactData],
    layout: &HashMap<String, Vec<String>>,
) -> DXLinkResult<Vec<MarketEvent>> {
    match decode(data, Some(layout)) {
        (events, None) => Ok(events),
        (_, Some(error)) => Err(error),
    }
}

/// One COMPACT row, addressed by field name rather than by position.
///
/// **This is the fix for the layout the server actually negotiates.** The
/// client asks for a field list, but a server may serve a *subset* of it: the
/// dxFeed demo drops `VWAP` from `Candle`, for instance. Reading by position
/// against the requested list then either shifts every value after the gap or,
/// if the arithmetic happens not to divide, drops the batch entirely.
///
/// Reading by name makes both harmless. A field the server left out reads as
/// "not provided" instead of as its neighbour, and a field it added or moved is
/// simply looked up where it really is.
struct Row<'a> {
    values: &'a [Value],
    /// Field name to column, built once per batch from the negotiated layout.
    columns: &'a HashMap<&'a str, usize>,
    event_type: &'a str,
    row: usize,
}

impl<'a> Row<'a> {
    fn value(&self, field: &str) -> Option<&'a Value> {
        self.columns.get(field).map(|index| &self.values[*index])
    }

    /// A column that must be text when present.
    ///
    /// Absent means the server does not serve it, which is an empty string
    /// rather than an error: the alternative is refusing an event over a field
    /// the consumer may not even read.
    fn text(&self, field: &str) -> DXLinkResult<String> {
        let Some(value) = self.value(field) else {
            return Ok(String::new());
        };
        value.as_str().map(str::to_string).ok_or_else(|| {
            DXLinkError::Protocol(format!(
                "{} row {}: field `{field}` should be a string, got {value}",
                self.event_type, self.row
            ))
        })
    }

    /// A column that has to be there for the event to mean anything.
    fn required_text(&self, field: &str) -> DXLinkResult<String> {
        let Some(value) = self.value(field) else {
            return Err(DXLinkError::Protocol(format!(
                "{} row {}: the negotiated layout has no `{field}` column, so the \
                 row cannot be identified",
                self.event_type, self.row
            )));
        };
        value.as_str().map(str::to_string).ok_or_else(|| {
            DXLinkError::Protocol(format!(
                "{} row {}: field `{field}` should be a string, got {value}",
                self.event_type, self.row
            ))
        })
    }

    /// A column that must be a whole number, such as an epoch millisecond.
    ///
    /// Absent reads as zero, which is what the server itself sends for a
    /// timestamp it did not populate.
    fn int(&self, field: &str) -> DXLinkResult<i64> {
        let Some(value) = self.value(field) else {
            return Ok(0);
        };
        value.as_i64().ok_or_else(|| {
            DXLinkError::Protocol(format!(
                "{} row {}: field `{field}` should be a whole number, got {value}",
                self.event_type, self.row
            ))
        })
    }

    /// A column that must be a double.
    ///
    /// Absent reads as `NaN`, the same value the protocol uses for a number
    /// that does not apply, so a consumer already has to handle it.
    fn double(&self, field: &str) -> DXLinkResult<f64> {
        let Some(value) = self.value(field) else {
            return Ok(f64::NAN);
        };
        as_json_double(value).ok_or_else(|| {
            DXLinkError::Protocol(format!(
                "{} row {}: field `{field}` should be a number, got {value}",
                self.event_type, self.row
            ))
        })
    }

    /// A column that must be a boolean.
    ///
    /// Strict when present: the protocol sends JSON `true`/`false` here, and
    /// treating `0` or `"false"` as false would turn a layout error into a
    /// plausible flag. Absent reads as `false`, which for `validTick` and the
    /// rest is the conservative answer.
    fn flag(&self, field: &str) -> DXLinkResult<bool> {
        let Some(value) = self.value(field) else {
            return Ok(false);
        };
        value.as_bool().ok_or_else(|| {
            DXLinkError::Protocol(format!(
                "{} row {}: field `{field}` should be a boolean, got {value}",
                self.event_type, self.row
            ))
        })
    }
}

/// Decodes what it can and reports the first problem it hit.
///
/// `layout` is the field list per event type that the channel **negotiated**,
/// which is not always the one it requested. `None` falls back to
/// [`EventType::compact_fields`], which is what the standalone
/// [`parse_compact_data`] entry points use when no channel is in play.
fn decode(
    data: &[CompactData],
    layout: Option<&HashMap<String, Vec<String>>>,
) -> (Vec<MarketEvent>, Option<DXLinkError>) {
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

        // The negotiated list wins. When a channel supplied one, a type missing
        // from it is refused rather than falling back to what this client
        // asked for: the fallback is the positional read that caused #63, and
        // guessing here would decode against a layout nobody agreed to.
        let fields: Vec<&str> = match layout {
            Some(layout) => match layout.get(header.as_str()) {
                Some(fields) => fields.iter().map(String::as_str).collect(),
                None => {
                    return (
                        events,
                        Some(DXLinkError::Protocol(format!(
                            "event type `{header}` is not in the layout this channel \
                             negotiated, so there is nothing to read its rows against"
                        ))),
                    );
                }
            },
            None => match event_type.compact_fields() {
                Some(fields) => fields.to_vec(),
                None => {
                    return (
                        events,
                        Some(DXLinkError::Protocol(format!(
                            "event type `{header}` has no decoder in this client, so its \
                             rows cannot be read"
                        ))),
                    );
                }
            },
        };

        if event_type.compact_fields().is_none() {
            return (
                events,
                Some(DXLinkError::Protocol(format!(
                    "event type `{header}` has no decoder in this client, so its rows \
                     cannot be read"
                ))),
            );
        }

        let stride = fields.len();
        if stride == 0 {
            return (
                events,
                Some(DXLinkError::Protocol(format!(
                    "{header}: the negotiated layout has no columns at all"
                ))),
            );
        }
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

        // Built once per batch, not per row.
        let mut columns: HashMap<&str, usize> = HashMap::with_capacity(stride);
        for (column, field) in fields.iter().enumerate() {
            // First wins. A duplicated name is the server's problem, and
            // picking one deterministically beats failing the batch.
            columns.entry(field).or_insert(column);
        }

        for (row, chunk) in values.chunks_exact(stride).enumerate() {
            let row = Row {
                values: chunk,
                columns: &columns,
                event_type: header,
                row,
            };

            // The batch header and the row's own eventType must agree, or the
            // row belongs to a layout other than the one being applied.
            let inner = match row.required_text("eventType") {
                Ok(inner) => inner,
                Err(e) => return (events, Some(e)),
            };
            if inner != *header {
                return (
                    events,
                    Some(DXLinkError::Protocol(format!(
                        "{header} row {}: the row says it is a `{inner}`, so it is not \
                         laid out the way this batch claims",
                        row.row
                    ))),
                );
            }

            match build_event(event_type, &row) {
                Ok(event) => events.push(event),
                Err(e) => return (events, Some(e)),
            }
        }
    }

    (events, None)
}

/// Builds one event from a row, reading every column by name.
fn build_event(event_type: EventType, row: &Row<'_>) -> DXLinkResult<MarketEvent> {
    let event_type_name = row.required_text("eventType")?;
    let symbol = row.required_text("eventSymbol")?;

    Ok(match event_type {
        EventType::Quote => MarketEvent::Quote(QuoteEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            bid_price: row.double("bidPrice")?,
            ask_price: row.double("askPrice")?,
            bid_size: row.double("bidSize")?,
            ask_size: row.double("askSize")?,
        }),
        EventType::Trade => MarketEvent::Trade(TradeEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            price: row.double("price")?,
            size: row.double("size")?,
            day_volume: row.double("dayVolume")?,
        }),
        EventType::Greeks => MarketEvent::Greeks(GreeksEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            delta: row.double("delta")?,
            gamma: row.double("gamma")?,
            theta: row.double("theta")?,
            vega: row.double("vega")?,
            rho: row.double("rho")?,
            volatility: row.double("volatility")?,
        }),
        EventType::Candle => MarketEvent::Candle(CandleEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            event_time: row.int("eventTime")?,
            event_flags: row.int("eventFlags")?,
            index: row.int("index")?,
            time: row.int("time")?,
            sequence: row.int("sequence")?,
            count: row.int("count")?,
            open: row.double("open")?,
            high: row.double("high")?,
            low: row.double("low")?,
            close: row.double("close")?,
            volume: row.double("volume")?,
            vwap: row.double("VWAP")?,
            bid_volume: row.double("bidVolume")?,
            ask_volume: row.double("askVolume")?,
            imp_volatility: row.double("impVolatility")?,
            open_interest: row.double("openInterest")?,
        }),
        EventType::Summary => MarketEvent::Summary(SummaryEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            event_time: row.int("eventTime")?,
            day_id: row.int("dayId")?,
            day_open_price: row.double("dayOpenPrice")?,
            day_high_price: row.double("dayHighPrice")?,
            day_low_price: row.double("dayLowPrice")?,
            day_close_price: row.double("dayClosePrice")?,
            day_close_price_type: row.text("dayClosePriceType")?,
            prev_day_id: row.int("prevDayId")?,
            prev_day_close_price: row.double("prevDayClosePrice")?,
            prev_day_close_price_type: row.text("prevDayClosePriceType")?,
            prev_day_volume: row.double("prevDayVolume")?,
            open_interest: row.double("openInterest")?,
        }),
        EventType::TimeAndSale => MarketEvent::TimeAndSale(TimeAndSaleEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            event_time: row.int("eventTime")?,
            event_flags: row.int("eventFlags")?,
            index: row.int("index")?,
            time: row.int("time")?,
            time_nano_part: row.int("timeNanoPart")?,
            sequence: row.int("sequence")?,
            exchange_code: row.text("exchangeCode")?,
            price: row.double("price")?,
            size: row.double("size")?,
            bid_price: row.double("bidPrice")?,
            ask_price: row.double("askPrice")?,
            exchange_sale_conditions: row.text("exchangeSaleConditions")?,
            trade_through_exempt: row.text("tradeThroughExempt")?,
            aggressor_side: row.text("aggressorSide")?,
            spread_leg: row.flag("spreadLeg")?,
            extended_trading_hours: row.flag("extendedTradingHours")?,
            valid_tick: row.flag("validTick")?,
            sale_type: row.text("type")?,
            buyer: row.text("buyer")?,
            seller: row.text("seller")?,
        }),
        EventType::Profile => MarketEvent::Profile(ProfileEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            event_time: row.int("eventTime")?,
            description: row.text("description")?,
            short_sale_restriction: row.text("shortSaleRestriction")?,
            trading_status: row.text("tradingStatus")?,
            status_reason: row.text("statusReason")?,
            halt_start_time: row.int("haltStartTime")?,
            halt_end_time: row.int("haltEndTime")?,
            high_limit_price: row.double("highLimitPrice")?,
            low_limit_price: row.double("lowLimitPrice")?,
            high_52_week_price: row.double("high52WeekPrice")?,
            low_52_week_price: row.double("low52WeekPrice")?,
            beta: row.double("beta")?,
            earnings_per_share: row.double("earningsPerShare")?,
            dividend_frequency: row.double("dividendFrequency")?,
            ex_dividend_amount: row.double("exDividendAmount")?,
            ex_dividend_day_id: row.int("exDividendDayId")?,
            shares: row.double("shares")?,
            free_float: row.double("freeFloat")?,
        }),
        EventType::Underlying => MarketEvent::Underlying(UnderlyingEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            event_time: row.int("eventTime")?,
            event_flags: row.int("eventFlags")?,
            index: row.int("index")?,
            time: row.int("time")?,
            sequence: row.int("sequence")?,
            volatility: row.double("volatility")?,
            front_volatility: row.double("frontVolatility")?,
            back_volatility: row.double("backVolatility")?,
            call_volume: row.double("callVolume")?,
            put_volume: row.double("putVolume")?,
            put_call_ratio: row.double("putCallRatio")?,
        }),
        EventType::TheoPrice => MarketEvent::TheoPrice(TheoPriceEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            event_time: row.int("eventTime")?,
            event_flags: row.int("eventFlags")?,
            index: row.int("index")?,
            time: row.int("time")?,
            sequence: row.int("sequence")?,
            price: row.double("price")?,
            underlying_price: row.double("underlyingPrice")?,
            delta: row.double("delta")?,
            gamma: row.double("gamma")?,
            dividend: row.double("dividend")?,
            interest: row.double("interest")?,
        }),
        EventType::Series => MarketEvent::Series(SeriesEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            event_time: row.int("eventTime")?,
            event_flags: row.int("eventFlags")?,
            index: row.int("index")?,
            time: row.int("time")?,
            sequence: row.int("sequence")?,
            expiration: row.int("expiration")?,
            volatility: row.double("volatility")?,
            call_volume: row.double("callVolume")?,
            put_volume: row.double("putVolume")?,
            put_call_ratio: row.double("putCallRatio")?,
            forward_price: row.double("forwardPrice")?,
            dividend: row.double("dividend")?,
            interest: row.double("interest")?,
        }),
        EventType::TradeETH => MarketEvent::TradeETH(TradeETHEvent {
            event_type: event_type_name,
            event_symbol: symbol,
            event_time: row.int("eventTime")?,
            time: row.int("time")?,
            time_nano_part: row.int("timeNanoPart")?,
            sequence: row.int("sequence")?,
            exchange_code: row.text("exchangeCode")?,
            price: row.double("price")?,
            change: row.double("change")?,
            size: row.double("size")?,
            day_id: row.int("dayId")?,
            day_volume: row.double("dayVolume")?,
            day_turnover: row.double("dayTurnover")?,
            tick_direction: row.text("tickDirection")?,
            extended_trading_hours: row.flag("extendedTradingHours")?,
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
        // Whichever type still has no decoder, rather than a name hardcoded
        // here. The previous version named Series and had to be edited the day
        // Series gained one, which is the wrong thing to spend an edit on.
        let Some(undecodable) = crate::events::ALL_EVENT_TYPES
            .iter()
            .find(|event_type| event_type.compact_fields().is_none())
        else {
            // Every declared type decodes. Nothing left to report, and this
            // test has nothing to say until the protocol adds one.
            return;
        };

        let name = undecodable.to_string();
        let error = try_parse_compact_data(&batch(&name, vec![json!(name.clone())]))
            .expect_err("a type with no decoder must be reported, not skipped");
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

#[cfg(test)]
mod candle_tests {
    use super::*;
    use serde_json::json;

    /// The exact 18 columns issue #24 specifies, in order.
    fn candle_row(symbol: &str) -> Vec<Value> {
        vec![
            json!("Candle"),             // 0 eventType
            json!(symbol),               // 1 eventSymbol
            json!(1_700_000_000_500i64), // 2 eventTime
            json!(0i64),                 // 3 eventFlags
            json!(7i64),                 // 4 index
            json!(1_700_000_000_000i64), // 5 time
            json!(3i64),                 // 6 sequence
            json!(42i64),                // 7 count
            json!(149.0),                // 8 open
            json!(151.0),                // 9 high
            json!(148.5),                // 10 low
            json!(150.5),                // 11 close
            json!(1_234_000.0),          // 12 volume
            json!(150.1),                // 13 VWAP
            json!(600_000.0),            // 14 bidVolume
            json!(634_000.0),            // 15 askVolume
            json!(0.31),                 // 16 impVolatility
            json!(4_200.0),              // 17 openInterest
        ]
    }

    fn batch(values: Vec<Value>) -> Vec<CompactData> {
        vec![
            CompactData::EventType("Candle".to_string()),
            CompactData::Values(values),
        ]
    }

    /// The requested field list and the decoder must agree on all 18 columns.
    #[test]
    fn test_the_field_list_matches_the_decoder_stride() {
        let fields = EventType::Candle
            .compact_fields()
            .expect("Candle has a decoder");
        assert_eq!(fields.len(), 18, "the layout is 18 columns");
        assert_eq!(fields[0], "eventType");
        assert_eq!(fields[1], "eventSymbol");
        assert_eq!(fields[13], "VWAP", "the wire name is upper case");
        assert_eq!(fields.len(), candle_row("AAPL").len());
    }

    #[test]
    fn test_two_candles_decode_with_the_right_stride() {
        let mut values = candle_row("AAPL{=5m}");
        values.extend(candle_row("MSFT{=5m}"));

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        assert_eq!(events.len(), 2);

        match (&events[0], &events[1]) {
            (MarketEvent::Candle(first), MarketEvent::Candle(second)) => {
                // The symbols prove the stride of eighteen.
                assert_eq!(first.event_symbol, "AAPL{=5m}");
                assert_eq!(second.event_symbol, "MSFT{=5m}");
                assert_eq!(first.event_time, 1_700_000_000_500);
                assert_eq!(first.event_flags, 0);
                assert_eq!(first.index, 7);
                assert_eq!(first.time, 1_700_000_000_000);
                assert_eq!(first.sequence, 3);
                assert_eq!(first.count, 42);
                assert_eq!(first.open, 149.0);
                assert_eq!(first.high, 151.0);
                assert_eq!(first.low, 148.5);
                assert_eq!(first.close, 150.5);
                assert_eq!(first.volume, 1_234_000.0);
                assert_eq!(first.vwap, 150.1);
                assert_eq!(first.bid_volume, 600_000.0);
                assert_eq!(first.ask_volume, 634_000.0);
                assert_eq!(first.imp_volatility, 0.31);
                assert_eq!(first.open_interest, 4_200.0);
            }
            other => panic!("expected two candles, got {other:?}"),
        }
    }

    /// eventFlags is what marks a historical snapshot's boundaries; a consumer
    /// that cannot see it cannot tell a snapshot from live updates.
    #[test]
    fn test_snapshot_flags_reach_the_consumer() {
        let mut values = candle_row("AAPL{=5m}");
        values[3] = json!(4i64); // SNAPSHOT_BEGIN

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        match &events[0] {
            MarketEvent::Candle(candle) => assert_eq!(candle.event_flags, 4),
            other => panic!("expected a candle, got {other:?}"),
        }
    }

    #[test]
    fn test_a_fractional_time_is_rejected() {
        let mut values = candle_row("AAPL{=5m}");
        values[5] = json!(1.5);

        let error = try_parse_compact_data(&batch(values)).expect_err("time is a whole number");
        let text = error.to_string();
        assert!(text.contains("time"), "the field is missing: {text}");
        assert!(text.contains("whole number"), "unclear: {text}");
    }

    /// An instrument with no implied volatility or open interest still decodes:
    /// the protocol sends NaN rather than dropping the columns.
    #[test]
    fn test_absent_optional_values_decode_as_nan() {
        let mut values = candle_row("AAPL{=5m}");
        values[16] = json!("NaN");
        values[17] = json!("NaN");

        let events = try_parse_compact_data(&batch(values)).expect("NaN is a value");
        match &events[0] {
            MarketEvent::Candle(candle) => {
                assert!(candle.imp_volatility.is_nan());
                assert!(candle.open_interest.is_nan());
                assert_eq!(candle.close, 150.5);
            }
            other => panic!("expected a candle, got {other:?}"),
        }
    }

    #[test]
    fn test_a_truncated_candle_row_is_rejected() {
        let mut values = candle_row("AAPL{=5m}");
        values.pop();
        assert!(try_parse_compact_data(&batch(values)).is_err());
    }

    #[test]
    fn test_a_candle_is_not_mistaken_for_another_event() {
        let events = try_parse_compact_data(&batch(candle_row("AAPL{=5m}"))).expect("well formed");
        assert!(matches!(events[0], MarketEvent::Candle(_)));

        let json = serde_json::to_string(&events[0]).expect("serialize");
        let back: MarketEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(
            matches!(back, MarketEvent::Candle(_)),
            "an untagged round trip picked the wrong variant: {back:?}"
        );
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    use serde_json::json;

    /// The exact 14 columns issue #25 specifies, in order.
    fn summary_row(symbol: &str) -> Vec<Value> {
        vec![
            json!("Summary"),            // 0 eventType
            json!(symbol),               // 1 eventSymbol
            json!(1_700_000_000_500i64), // 2 eventTime
            json!(20100i64),             // 3 dayId
            json!(149.5),                // 4 dayOpenPrice
            json!(152.0),                // 5 dayHighPrice
            json!(148.0),                // 6 dayLowPrice
            json!(150.75),               // 7 dayClosePrice
            json!("Final"),              // 8 dayClosePriceType
            json!(20099i64),             // 9 prevDayId
            json!(147.75),               // 10 prevDayClosePrice
            json!("Final"),              // 11 prevDayClosePriceType
            json!(58_000_000.0),         // 12 prevDayVolume
            json!(4_200.0),              // 13 openInterest
        ]
    }

    fn batch(values: Vec<Value>) -> Vec<CompactData> {
        vec![
            CompactData::EventType("Summary".to_string()),
            CompactData::Values(values),
        ]
    }

    #[test]
    fn test_the_field_list_matches_the_decoder_stride() {
        let fields = EventType::Summary
            .compact_fields()
            .expect("Summary has a decoder");
        assert_eq!(fields.len(), 14, "the layout is 14 columns");
        assert_eq!(fields.len(), summary_row("AAPL").len());
    }

    #[test]
    fn test_two_summaries_decode_with_the_right_stride() {
        let mut values = summary_row("AAPL");
        values.extend(summary_row("MSFT"));

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        assert_eq!(events.len(), 2);

        match (&events[0], &events[1]) {
            (MarketEvent::Summary(first), MarketEvent::Summary(second)) => {
                // The symbols prove the stride of fourteen.
                assert_eq!(first.event_symbol, "AAPL");
                assert_eq!(second.event_symbol, "MSFT");
                assert_eq!(first.event_time, 1_700_000_000_500);
                assert_eq!(first.day_id, 20100);
                assert_eq!(first.day_open_price, 149.5);
                assert_eq!(first.day_high_price, 152.0);
                assert_eq!(first.day_low_price, 148.0);
                assert_eq!(first.day_close_price, 150.75);
                assert_eq!(first.day_close_price_type, "Final");
                assert_eq!(first.prev_day_id, 20099);
                assert_eq!(first.prev_day_close_price, 147.75);
                assert_eq!(first.prev_day_close_price_type, "Final");
                assert_eq!(first.prev_day_volume, 58_000_000.0);
                assert_eq!(first.open_interest, 4_200.0);
            }
            other => panic!("expected two summaries, got {other:?}"),
        }
    }

    /// The close price type is what stops a consumer treating a provisional
    /// close as settled.
    #[test]
    fn test_a_provisional_close_is_distinguishable() {
        let mut values = summary_row("AAPL");
        values[8] = json!("Preliminary");

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        match &events[0] {
            MarketEvent::Summary(summary) => {
                assert_eq!(summary.day_close_price_type, "Preliminary");
                assert_eq!(summary.prev_day_close_price_type, "Final");
            }
            other => panic!("expected a summary, got {other:?}"),
        }
    }

    #[test]
    fn test_a_numeric_close_price_type_is_rejected() {
        let mut values = summary_row("AAPL");
        values[8] = json!(1.0);

        let error = try_parse_compact_data(&batch(values)).expect_err("the type is text");
        assert!(
            error.to_string().contains("dayClosePriceType"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_an_instrument_without_open_interest_decodes() {
        // Equities have no open interest; the protocol sends NaN rather than
        // omitting the column.
        let mut values = summary_row("AAPL");
        values[13] = json!("NaN");

        let events = try_parse_compact_data(&batch(values)).expect("NaN is a value");
        match &events[0] {
            MarketEvent::Summary(summary) => assert!(summary.open_interest.is_nan()),
            other => panic!("expected a summary, got {other:?}"),
        }
    }

    #[test]
    fn test_a_truncated_summary_row_is_rejected() {
        let mut values = summary_row("AAPL");
        values.pop();
        assert!(try_parse_compact_data(&batch(values)).is_err());
    }

    #[test]
    fn test_a_summary_is_not_mistaken_for_another_event() {
        let events = try_parse_compact_data(&batch(summary_row("AAPL"))).expect("well formed");
        assert!(matches!(events[0], MarketEvent::Summary(_)));

        let json = serde_json::to_string(&events[0]).expect("serialize");
        let back: MarketEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(
            matches!(back, MarketEvent::Summary(_)),
            "an untagged round trip picked the wrong variant: {back:?}"
        );
    }
}

#[cfg(test)]
mod time_and_sale_tests {
    use super::*;
    use serde_json::json;

    /// The exact 22 columns issue #26 specifies, in order.
    fn print_row(symbol: &str) -> Vec<Value> {
        vec![
            json!("TimeAndSale"),        // 0 eventType
            json!(symbol),               // 1 eventSymbol
            json!(1_700_000_000_500i64), // 2 eventTime
            json!(0i64),                 // 3 eventFlags
            json!(11i64),                // 4 index
            json!(1_700_000_000_000i64), // 5 time
            json!(250_000i64),           // 6 timeNanoPart
            json!(3i64),                 // 7 sequence
            json!("Q"),                  // 8 exchangeCode
            json!(151.25),               // 9 price
            json!(75.0),                 // 10 size
            json!(151.2),                // 11 bidPrice
            json!(151.3),                // 12 askPrice
            json!("@ TI"),               // 13 exchangeSaleConditions
            json!("X"),                  // 14 tradeThroughExempt
            json!("Buy"),                // 15 aggressorSide
            json!(false),                // 16 spreadLeg
            json!(true),                 // 17 extendedTradingHours
            json!(true),                 // 18 validTick
            json!("NEW"),                // 19 type
            json!("NSDQ"),               // 20 buyer
            json!("NYSE"),               // 21 seller
        ]
    }

    fn batch(values: Vec<Value>) -> Vec<CompactData> {
        vec![
            CompactData::EventType("TimeAndSale".to_string()),
            CompactData::Values(values),
        ]
    }

    #[test]
    fn test_the_field_list_matches_the_decoder_stride() {
        let fields = EventType::TimeAndSale
            .compact_fields()
            .expect("TimeAndSale has a decoder");
        assert_eq!(fields.len(), 22, "the layout is 22 columns");
        assert_eq!(fields.len(), print_row("AAPL").len());
    }

    #[test]
    fn test_two_prints_decode_with_the_right_stride() {
        let mut values = print_row("AAPL");
        values.extend(print_row("MSFT"));

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        assert_eq!(events.len(), 2);

        match (&events[0], &events[1]) {
            (MarketEvent::TimeAndSale(first), MarketEvent::TimeAndSale(second)) => {
                // The symbols prove the stride of twenty-two.
                assert_eq!(first.event_symbol, "AAPL");
                assert_eq!(second.event_symbol, "MSFT");
                assert_eq!(first.event_time, 1_700_000_000_500);
                assert_eq!(first.event_flags, 0);
                assert_eq!(first.index, 11);
                assert_eq!(first.time, 1_700_000_000_000);
                assert_eq!(first.time_nano_part, 250_000);
                assert_eq!(first.sequence, 3);
                assert_eq!(first.exchange_code, "Q");
                assert_eq!(first.price, 151.25);
                assert_eq!(first.size, 75.0);
                assert_eq!(first.bid_price, 151.2);
                assert_eq!(first.ask_price, 151.3);
                assert_eq!(first.exchange_sale_conditions, "@ TI");
                assert_eq!(first.trade_through_exempt, "X");
                assert_eq!(first.aggressor_side, "Buy");
                assert!(!first.spread_leg);
                assert!(first.extended_trading_hours);
                assert!(first.valid_tick);
                assert_eq!(first.sale_type, "NEW");
                assert_eq!(first.buyer, "NSDQ");
                assert_eq!(first.seller, "NYSE");
            }
            other => panic!("expected two prints, got {other:?}"),
        }
    }

    /// The three booleans sit next to each other, so a one-column shift keeps
    /// decoding and only changes which flag is which.
    #[test]
    fn test_the_flags_keep_their_own_columns() {
        let mut values = print_row("AAPL");
        values[16] = json!(true); // spreadLeg
        values[17] = json!(false); // extendedTradingHours
        values[18] = json!(false); // validTick

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        match &events[0] {
            MarketEvent::TimeAndSale(print) => {
                assert!(print.spread_leg);
                assert!(!print.extended_trading_hours);
                assert!(!print.valid_tick);
            }
            other => panic!("expected a print, got {other:?}"),
        }
    }

    #[test]
    fn test_a_numeric_flag_is_rejected() {
        let mut values = print_row("AAPL");
        // A layout that shifted a price into a flag column would land here.
        values[18] = json!(1);

        let error = try_parse_compact_data(&batch(values)).expect_err("the column is a boolean");
        assert!(
            error.to_string().contains("validTick"),
            "the field is missing: {error}"
        );
        assert!(
            error.to_string().contains("boolean"),
            "the expected type is missing: {error}"
        );
    }

    #[test]
    fn test_a_numeric_sale_condition_is_rejected() {
        let mut values = print_row("AAPL");
        values[13] = json!(4.0);

        let error = try_parse_compact_data(&batch(values)).expect_err("the column is text");
        assert!(
            error.to_string().contains("exchangeSaleConditions"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_a_print_without_a_surrounding_quote_decodes() {
        // Off-exchange prints arrive with no bid or ask; the protocol sends NaN
        // rather than omitting the columns.
        let mut values = print_row("AAPL");
        values[11] = json!("NaN");
        values[12] = json!("NaN");

        let events = try_parse_compact_data(&batch(values)).expect("NaN is a value");
        match &events[0] {
            MarketEvent::TimeAndSale(print) => {
                assert!(print.bid_price.is_nan());
                assert!(print.ask_price.is_nan());
                // The price itself still has to be a real number.
                assert_eq!(print.price, 151.25);
            }
            other => panic!("expected a print, got {other:?}"),
        }
    }

    #[test]
    fn test_a_truncated_print_row_is_rejected() {
        let mut values = print_row("AAPL");
        values.pop();
        assert!(try_parse_compact_data(&batch(values)).is_err());
    }

    #[test]
    fn test_a_print_is_not_mistaken_for_another_event() {
        let events = try_parse_compact_data(&batch(print_row("AAPL"))).expect("well formed");
        assert!(matches!(events[0], MarketEvent::TimeAndSale(_)));

        let json = serde_json::to_string(&events[0]).expect("serialize");
        // The wire names have to survive the round trip, `type` included.
        assert!(
            json.contains("\"type\":\"NEW\""),
            "wrong field name: {json}"
        );
        assert!(
            json.contains("\"exchangeSaleConditions\""),
            "wrong field name: {json}"
        );

        let back: MarketEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(
            matches!(back, MarketEvent::TimeAndSale(_)),
            "an untagged round trip picked the wrong variant: {back:?}"
        );
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    use serde_json::json;

    /// The exact 20 columns issue #27 specifies, in order.
    fn profile_row(symbol: &str) -> Vec<Value> {
        vec![
            json!("Profile"),                   // 0 eventType
            json!(symbol),                      // 1 eventSymbol
            json!(1_700_000_000_500i64),        // 2 eventTime
            json!("Apple Inc. - Common Stock"), // 3 description
            json!("Inactive"),                  // 4 shortSaleRestriction
            json!("Halted"),                    // 5 tradingStatus
            json!("News pending"),              // 6 statusReason
            json!(1_700_000_100_000i64),        // 7 haltStartTime
            json!(1_700_000_900_000i64),        // 8 haltEndTime
            json!(165.0),                       // 9 highLimitPrice
            json!(135.0),                       // 10 lowLimitPrice
            json!(199.62),                      // 11 high52WeekPrice
            json!(124.17),                      // 12 low52WeekPrice
            json!(1.29),                        // 13 beta
            json!(6.13),                        // 14 earningsPerShare
            json!(4.0),                         // 15 dividendFrequency
            json!(0.24),                        // 16 exDividendAmount
            json!(20050i64),                    // 17 exDividendDayId
            json!(15_552_800_000.0),            // 18 shares
            json!(15_461_900_000.0),            // 19 freeFloat
        ]
    }

    fn batch(values: Vec<Value>) -> Vec<CompactData> {
        vec![
            CompactData::EventType("Profile".to_string()),
            CompactData::Values(values),
        ]
    }

    #[test]
    fn test_the_field_list_matches_the_decoder_stride() {
        let fields = EventType::Profile
            .compact_fields()
            .expect("Profile has a decoder");
        assert_eq!(fields.len(), 20, "the layout is 20 columns");
        assert_eq!(fields.len(), profile_row("AAPL").len());
    }

    #[test]
    fn test_two_profiles_decode_with_the_right_stride() {
        let mut values = profile_row("AAPL");
        values.extend(profile_row("MSFT"));

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        assert_eq!(events.len(), 2);

        match (&events[0], &events[1]) {
            (MarketEvent::Profile(first), MarketEvent::Profile(second)) => {
                // The symbols prove the stride of twenty.
                assert_eq!(first.event_symbol, "AAPL");
                assert_eq!(second.event_symbol, "MSFT");
                assert_eq!(first.event_time, 1_700_000_000_500);
                assert_eq!(first.description, "Apple Inc. - Common Stock");
                assert_eq!(first.short_sale_restriction, "Inactive");
                assert_eq!(first.trading_status, "Halted");
                assert_eq!(first.status_reason, "News pending");
                assert_eq!(first.halt_start_time, 1_700_000_100_000);
                assert_eq!(first.halt_end_time, 1_700_000_900_000);
                assert_eq!(first.high_limit_price, 165.0);
                assert_eq!(first.low_limit_price, 135.0);
                assert_eq!(first.high_52_week_price, 199.62);
                assert_eq!(first.low_52_week_price, 124.17);
                assert_eq!(first.beta, 1.29);
                assert_eq!(first.earnings_per_share, 6.13);
                assert_eq!(first.dividend_frequency, 4.0);
                assert_eq!(first.ex_dividend_amount, 0.24);
                assert_eq!(first.ex_dividend_day_id, 20050);
                assert_eq!(first.shares, 15_552_800_000.0);
                assert_eq!(first.free_float, 15_461_900_000.0);
            }
            other => panic!("expected two profiles, got {other:?}"),
        }
    }

    /// The four text columns sit next to each other, so a one-column shift
    /// keeps decoding and only changes which string is which.
    #[test]
    fn test_the_status_columns_keep_their_own_meaning() {
        let mut values = profile_row("AAPL");
        values[4] = json!("Active");
        values[5] = json!("Active");
        values[6] = json!("");

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        match &events[0] {
            MarketEvent::Profile(profile) => {
                assert_eq!(profile.short_sale_restriction, "Active");
                assert_eq!(profile.trading_status, "Active");
                // A trading instrument has no reason to report, and an empty
                // string is a value rather than a missing column.
                assert_eq!(profile.status_reason, "");
                assert_eq!(profile.description, "Apple Inc. - Common Stock");
            }
            other => panic!("expected a profile, got {other:?}"),
        }
    }

    #[test]
    fn test_a_numeric_trading_status_is_rejected() {
        let mut values = profile_row("AAPL");
        values[5] = json!(1.0);

        let error = try_parse_compact_data(&batch(values)).expect_err("the column is text");
        assert!(
            error.to_string().contains("tradingStatus"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_a_textual_limit_price_is_rejected() {
        let mut values = profile_row("AAPL");
        // "165.0" as text is what a shifted layout looks like, and it is not a
        // JSONDouble special value, so it must not decode.
        values[9] = json!("165.0");

        let error = try_parse_compact_data(&batch(values)).expect_err("the column is a number");
        assert!(
            error.to_string().contains("highLimitPrice"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_an_instrument_without_fundamentals_decodes() {
        // An index has no earnings, dividends or float; the protocol sends NaN
        // rather than omitting the columns.
        let mut values = profile_row("SPX");
        values[13] = json!("NaN");
        values[14] = json!("NaN");
        values[18] = json!("NaN");
        values[19] = json!("NaN");

        let events = try_parse_compact_data(&batch(values)).expect("NaN is a value");
        match &events[0] {
            MarketEvent::Profile(profile) => {
                assert!(profile.beta.is_nan());
                assert!(profile.earnings_per_share.is_nan());
                assert!(profile.shares.is_nan());
                assert!(profile.free_float.is_nan());
                // The limits still have to be real numbers.
                assert_eq!(profile.high_limit_price, 165.0);
            }
            other => panic!("expected a profile, got {other:?}"),
        }
    }

    /// The venue reports no upper limit as positive infinity, which JSON has no
    /// literal for.
    #[test]
    fn test_an_unbounded_limit_price_decodes() {
        let mut values = profile_row("AAPL");
        values[9] = json!("Infinity");
        values[10] = json!("-Infinity");

        let events = try_parse_compact_data(&batch(values)).expect("Infinity is a value");
        match &events[0] {
            MarketEvent::Profile(profile) => {
                assert_eq!(profile.high_limit_price, f64::INFINITY);
                assert_eq!(profile.low_limit_price, f64::NEG_INFINITY);
            }
            other => panic!("expected a profile, got {other:?}"),
        }
    }

    #[test]
    fn test_a_truncated_profile_row_is_rejected() {
        let mut values = profile_row("AAPL");
        values.pop();
        assert!(try_parse_compact_data(&batch(values)).is_err());
    }

    #[test]
    fn test_a_profile_is_not_mistaken_for_another_event() {
        let events = try_parse_compact_data(&batch(profile_row("AAPL"))).expect("well formed");
        assert!(matches!(events[0], MarketEvent::Profile(_)));

        let json = serde_json::to_string(&events[0]).expect("serialize");
        // Every camelCase wire name has to survive the round trip.
        for field in [
            "eventType",
            "eventSymbol",
            "eventTime",
            "description",
            "shortSaleRestriction",
            "tradingStatus",
            "statusReason",
            "haltStartTime",
            "haltEndTime",
            "highLimitPrice",
            "lowLimitPrice",
            "high52WeekPrice",
            "low52WeekPrice",
            "beta",
            "earningsPerShare",
            "dividendFrequency",
            "exDividendAmount",
            "exDividendDayId",
            "shares",
            "freeFloat",
        ] {
            assert!(
                json.contains(&format!("\"{field}\":")),
                "wire name `{field}` is missing: {json}"
            );
        }

        let back: MarketEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(
            matches!(back, MarketEvent::Profile(_)),
            "an untagged round trip picked the wrong variant: {back:?}"
        );
    }
}

#[cfg(test)]
mod underlying_tests {
    use super::*;
    use serde_json::json;

    /// The exact 13 columns issue #28 specifies, in order.
    fn underlying_row(symbol: &str) -> Vec<Value> {
        vec![
            json!("Underlying"),         // 0 eventType
            json!(symbol),               // 1 eventSymbol
            json!(1_700_000_000_500i64), // 2 eventTime
            json!(0i64),                 // 3 eventFlags
            json!(9i64),                 // 4 index
            json!(1_700_000_000_000i64), // 5 time
            json!(3i64),                 // 6 sequence
            json!(0.25),                 // 7 volatility
            json!(0.28),                 // 8 frontVolatility
            json!(0.22),                 // 9 backVolatility
            json!(310_000.0),            // 10 callVolume
            json!(465_000.0),            // 11 putVolume
            json!(1.5),                  // 12 putCallRatio
        ]
    }

    fn batch(values: Vec<Value>) -> Vec<CompactData> {
        vec![
            CompactData::EventType("Underlying".to_string()),
            CompactData::Values(values),
        ]
    }

    #[test]
    fn test_the_field_list_matches_the_decoder_stride() {
        let fields = EventType::Underlying
            .compact_fields()
            .expect("Underlying has a decoder");
        assert_eq!(fields.len(), 13, "the layout is 13 columns");
        assert_eq!(fields.len(), underlying_row("SPX").len());
    }

    #[test]
    fn test_two_underlyings_decode_with_the_right_stride() {
        let mut values = underlying_row("SPX");
        values.extend(underlying_row("NDX"));

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        assert_eq!(events.len(), 2);

        match (&events[0], &events[1]) {
            (MarketEvent::Underlying(first), MarketEvent::Underlying(second)) => {
                // The symbols prove the stride of thirteen.
                assert_eq!(first.event_symbol, "SPX");
                assert_eq!(second.event_symbol, "NDX");
                assert_eq!(first.event_time, 1_700_000_000_500);
                assert_eq!(first.event_flags, 0);
                assert_eq!(first.index, 9);
                assert_eq!(first.time, 1_700_000_000_000);
                assert_eq!(first.sequence, 3);
                assert_eq!(first.volatility, 0.25);
                assert_eq!(first.front_volatility, 0.28);
                assert_eq!(first.back_volatility, 0.22);
                assert_eq!(first.call_volume, 310_000.0);
                assert_eq!(first.put_volume, 465_000.0);
                assert_eq!(first.put_call_ratio, 1.5);
            }
            other => panic!("expected two underlyings, got {other:?}"),
        }
    }

    /// The three volatilities sit next to each other and are all plausible
    /// fractions, so a one-column shift there changes the term structure
    /// without looking wrong.
    #[test]
    fn test_the_term_structure_keeps_its_own_columns() {
        let mut values = underlying_row("SPX");
        values[7] = json!(0.30);
        values[8] = json!(0.40);
        values[9] = json!(0.20);

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        match &events[0] {
            MarketEvent::Underlying(surface) => {
                assert_eq!(surface.volatility, 0.30);
                assert_eq!(surface.front_volatility, 0.40);
                assert_eq!(surface.back_volatility, 0.20);
                // Backwardation: the front is above the back, which is the case
                // a shifted layout would hide.
                assert!(surface.front_volatility > surface.back_volatility);
            }
            other => panic!("expected an underlying, got {other:?}"),
        }
    }

    #[test]
    fn test_a_textual_volatility_is_rejected() {
        let mut values = underlying_row("SPX");
        // Not a JSONDouble special value, so it must not decode.
        values[8] = json!("high");

        let error = try_parse_compact_data(&batch(values)).expect_err("the column is a number");
        assert!(
            error.to_string().contains("frontVolatility"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_a_fractional_sequence_is_rejected() {
        let mut values = underlying_row("SPX");
        values[6] = json!(3.5);

        let error =
            try_parse_compact_data(&batch(values)).expect_err("the column is a whole number");
        assert!(
            error.to_string().contains("sequence"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_an_underlying_without_options_traded_decodes() {
        // With no volume on either side the ratio is undefined, and the protocol
        // sends NaN rather than omitting the column.
        let mut values = underlying_row("SPX");
        values[10] = json!(0.0);
        values[11] = json!(0.0);
        values[12] = json!("NaN");

        let events = try_parse_compact_data(&batch(values)).expect("NaN is a value");
        match &events[0] {
            MarketEvent::Underlying(surface) => {
                assert_eq!(surface.call_volume, 0.0);
                assert_eq!(surface.put_volume, 0.0);
                assert!(surface.put_call_ratio.is_nan());
            }
            other => panic!("expected an underlying, got {other:?}"),
        }
    }

    #[test]
    fn test_a_truncated_underlying_row_is_rejected() {
        let mut values = underlying_row("SPX");
        values.pop();
        assert!(try_parse_compact_data(&batch(values)).is_err());
    }

    #[test]
    fn test_an_underlying_is_not_mistaken_for_another_event() {
        let events = try_parse_compact_data(&batch(underlying_row("SPX"))).expect("well formed");
        assert!(matches!(events[0], MarketEvent::Underlying(_)));

        let json = serde_json::to_string(&events[0]).expect("serialize");
        let back: MarketEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(
            matches!(back, MarketEvent::Underlying(_)),
            "an untagged round trip picked the wrong variant: {back:?}"
        );
    }
}

#[cfg(test)]
mod theo_price_tests {
    use super::*;
    use serde_json::json;

    /// The exact 13 columns issue #29 specifies, in order.
    fn theo_row(symbol: &str) -> Vec<Value> {
        vec![
            json!("TheoPrice"),          // 0 eventType
            json!(symbol),               // 1 eventSymbol
            json!(1_700_000_000_500i64), // 2 eventTime
            json!(0i64),                 // 3 eventFlags
            json!(5i64),                 // 4 index
            json!(1_700_000_000_000i64), // 5 time
            json!(3i64),                 // 6 sequence
            json!(4.35),                 // 7 price
            json!(152.4),                // 8 underlyingPrice
            json!(0.65),                 // 9 delta
            json!(0.05),                 // 10 gamma
            json!(0.55),                 // 11 dividend
            json!(4.75),                 // 12 interest
        ]
    }

    fn batch(values: Vec<Value>) -> Vec<CompactData> {
        vec![
            CompactData::EventType("TheoPrice".to_string()),
            CompactData::Values(values),
        ]
    }

    #[test]
    fn test_the_field_list_matches_the_decoder_stride() {
        let fields = EventType::TheoPrice
            .compact_fields()
            .expect("TheoPrice has a decoder");
        assert_eq!(fields.len(), 13, "the layout is 13 columns");
        assert_eq!(fields.len(), theo_row("AAPL240119C00150000").len());
    }

    #[test]
    fn test_two_theo_prices_decode_with_the_right_stride() {
        let mut values = theo_row("AAPL240119C00150000");
        values.extend(theo_row("AAPL240119P00150000"));

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        assert_eq!(events.len(), 2);

        match (&events[0], &events[1]) {
            (MarketEvent::TheoPrice(first), MarketEvent::TheoPrice(second)) => {
                // The symbols prove the stride of thirteen.
                assert_eq!(first.event_symbol, "AAPL240119C00150000");
                assert_eq!(second.event_symbol, "AAPL240119P00150000");
                assert_eq!(first.event_time, 1_700_000_000_500);
                assert_eq!(first.event_flags, 0);
                assert_eq!(first.index, 5);
                assert_eq!(first.time, 1_700_000_000_000);
                assert_eq!(first.sequence, 3);
                assert_eq!(first.price, 4.35);
                assert_eq!(first.underlying_price, 152.4);
                assert_eq!(first.delta, 0.65);
                assert_eq!(first.gamma, 0.05);
                assert_eq!(first.dividend, 0.55);
                assert_eq!(first.interest, 4.75);
            }
            other => panic!("expected two theoretical prices, got {other:?}"),
        }
    }

    /// The option price and the underlying price are adjacent and both are
    /// prices, so a one-column shift there is the drift that looks most
    /// plausible on a screen.
    #[test]
    fn test_the_option_price_is_not_the_underlying_price() {
        let events =
            try_parse_compact_data(&batch(theo_row("AAPL240119C00150000"))).expect("well formed");
        match &events[0] {
            MarketEvent::TheoPrice(theo) => {
                assert_eq!(theo.price, 4.35, "the option price is the cheap one");
                assert_eq!(theo.underlying_price, 152.4);
                assert!(
                    theo.price < theo.underlying_price,
                    "a shifted layout would swap these two"
                );
            }
            other => panic!("expected a theoretical price, got {other:?}"),
        }
    }

    #[test]
    fn test_a_textual_price_is_rejected() {
        let mut values = theo_row("AAPL240119C00150000");
        // Not a JSONDouble special value, so it must not decode.
        values[7] = json!("4.35");

        let error = try_parse_compact_data(&batch(values)).expect_err("the column is a number");
        assert!(
            error.to_string().contains("price"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_a_fractional_index_is_rejected() {
        let mut values = theo_row("AAPL240119C00150000");
        values[4] = json!(5.5);

        let error =
            try_parse_compact_data(&batch(values)).expect_err("the column is a whole number");
        assert!(
            error.to_string().contains("index"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_an_option_with_no_dividend_decodes() {
        // A non-dividend-paying underlying reports NaN rather than zero, and
        // the difference matters to whoever re-prices from these inputs.
        let mut values = theo_row("AAPL240119C00150000");
        values[11] = json!("NaN");

        let events = try_parse_compact_data(&batch(values)).expect("NaN is a value");
        match &events[0] {
            MarketEvent::TheoPrice(theo) => {
                assert!(theo.dividend.is_nan());
                assert_eq!(theo.interest, 4.75);
            }
            other => panic!("expected a theoretical price, got {other:?}"),
        }
    }

    #[test]
    fn test_a_truncated_theo_price_row_is_rejected() {
        let mut values = theo_row("AAPL240119C00150000");
        values.pop();
        assert!(try_parse_compact_data(&batch(values)).is_err());
    }

    #[test]
    fn test_a_theo_price_is_not_mistaken_for_another_event() {
        let events =
            try_parse_compact_data(&batch(theo_row("AAPL240119C00150000"))).expect("well formed");
        assert!(matches!(events[0], MarketEvent::TheoPrice(_)));

        let json = serde_json::to_string(&events[0]).expect("serialize");
        let back: MarketEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(
            matches!(back, MarketEvent::TheoPrice(_)),
            "an untagged round trip picked the wrong variant: {back:?}"
        );
    }
}

#[cfg(test)]
mod trade_eth_tests {
    use super::*;
    use serde_json::json;

    /// A row exactly as the demo server sent it for AAPL, values included.
    /// Copied from a capture rather than invented, because the point of this
    /// type is that its layout was confirmed rather than inferred from `Trade`.
    fn print_row(symbol: &str) -> Vec<Value> {
        vec![
            json!("TradeETH"),           // 0 eventType
            json!(symbol),               // 1 eventSymbol
            json!(0i64),                 // 2 eventTime, sent as 0 by a live feed
            json!(1_785_542_396_498i64), // 3 time
            json!(0i64),                 // 4 timeNanoPart
            json!(3_009_441i64),         // 5 sequence
            json!("D"),                  // 6 exchangeCode
            json!(307.3554),             // 7 price
            json!("NaN"),                // 8 change
            json!(600.0),                // 9 size
            json!(20665i64),             // 10 dayId
            json!(11_085_372.061_196),   // 11 dayVolume
            json!(3_417_052_245.055_67), // 12 dayTurnover
            json!("UNDEFINED"),          // 13 tickDirection
            json!(true),                 // 14 extendedTradingHours
        ]
    }

    fn batch(values: Vec<Value>) -> Vec<CompactData> {
        vec![
            CompactData::EventType("TradeETH".to_string()),
            CompactData::Values(values),
        ]
    }

    #[test]
    fn test_the_field_list_matches_the_decoder_stride() {
        let fields = EventType::TradeETH
            .compact_fields()
            .expect("TradeETH has a decoder");
        assert_eq!(fields.len(), 15, "the layout the server confirmed is 15");
        assert_eq!(fields.len(), print_row("AAPL").len());
    }

    #[test]
    fn test_a_real_row_decodes_field_for_field() {
        let events = try_parse_compact_data(&batch(print_row("AAPL"))).expect("well formed");
        match &events[0] {
            MarketEvent::TradeETH(print) => {
                assert_eq!(print.event_symbol, "AAPL");
                assert_eq!(print.event_time, 0);
                assert_eq!(print.time, 1_785_542_396_498);
                assert_eq!(print.time_nano_part, 0);
                assert_eq!(print.sequence, 3_009_441);
                assert_eq!(print.exchange_code, "D");
                assert_eq!(print.price, 307.3554);
                assert!(print.change.is_nan(), "the venue sends NaN here");
                assert_eq!(print.size, 600.0);
                assert_eq!(print.day_id, 20665);
                assert_eq!(print.day_volume, 11_085_372.061_196);
                assert_eq!(print.day_turnover, 3_417_052_245.055_67);
                assert_eq!(print.tick_direction, "UNDEFINED");
                // The whole reason the type exists.
                assert!(print.extended_trading_hours);
            }
            other => panic!("expected an extended-hours print, got {other:?}"),
        }
    }

    #[test]
    fn test_two_prints_decode_with_the_right_stride() {
        let mut values = print_row("AAPL");
        values.extend(print_row("MSFT"));

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        assert_eq!(events.len(), 2);
        match (&events[0], &events[1]) {
            (MarketEvent::TradeETH(first), MarketEvent::TradeETH(second)) => {
                assert_eq!(first.event_symbol, "AAPL");
                assert_eq!(second.event_symbol, "MSFT");
            }
            other => panic!("expected two prints, got {other:?}"),
        }
    }

    /// The point of issue #66: an undecodable type does not merely go missing,
    /// it abandons the rest of the batch it appears in.
    #[test]
    fn test_a_batch_mixing_trade_and_trade_eth_decodes_whole() {
        let mut data = batch(print_row("AAPL"));
        data.push(CompactData::EventType("Trade".to_string()));
        data.push(CompactData::Values(vec![
            json!("Trade"),
            json!("MSFT"),
            json!(464.72),
            json!(100.0),
            json!(60_845_971.0),
        ]));

        let events = try_parse_compact_data(&data).expect("both types decode now");
        assert_eq!(
            events.len(),
            2,
            "a TradeETH row used to abort the batch and take the Trade with it"
        );
        assert!(matches!(events[0], MarketEvent::TradeETH(_)));
        assert!(matches!(events[1], MarketEvent::Trade(_)));
    }

    #[test]
    fn test_a_numeric_tick_direction_is_rejected() {
        let mut values = print_row("AAPL");
        values[13] = json!(1.0);

        let error = try_parse_compact_data(&batch(values)).expect_err("the column is text");
        assert!(
            error.to_string().contains("tickDirection"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_a_regular_hours_print_is_distinguishable() {
        let mut values = print_row("AAPL");
        values[14] = json!(false);

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        match &events[0] {
            MarketEvent::TradeETH(print) => assert!(!print.extended_trading_hours),
            other => panic!("expected a print, got {other:?}"),
        }
    }

    #[test]
    fn test_a_truncated_print_row_is_rejected() {
        let mut values = print_row("AAPL");
        values.pop();
        assert!(try_parse_compact_data(&batch(values)).is_err());
    }
}

#[cfg(test)]
mod series_tests {
    use super::*;
    use serde_json::json;

    /// One expiration exactly as the demo server sent it for AAPL. Captured
    /// rather than invented: the issue asks for the layout to be confirmed
    /// against a real row, not extrapolated from Underlying or TheoPrice.
    fn series_row(symbol: &str, expiration: i64) -> Vec<Value> {
        vec![
            json!("Series"),                // 0 eventType
            json!(symbol),                  // 1 eventSymbol
            json!(0i64),                    // 2 eventTime, sent as 0 by a live feed
            json!(4i64),                    // 3 eventFlags, the snapshot bit
            json!(23i64),                   // 4 index
            json!(1_785_542_361_974i64),    // 5 time
            json!(0i64),                    // 6 sequence
            json!(expiration),              // 7 expiration
            json!(0.3188),                  // 8 volatility
            json!(8526.0),                  // 9 callVolume
            json!(1899.0),                  // 10 putVolume
            json!(0.222_730_471_498_944_4), // 11 putCallRatio
            json!(335.174_395_879_884),     // 12 forwardPrice
            json!(0.0),                     // 13 dividend
            json!(0.0),                     // 14 interest
        ]
    }

    fn batch(values: Vec<Value>) -> Vec<CompactData> {
        vec![
            CompactData::EventType("Series".to_string()),
            CompactData::Values(values),
        ]
    }

    #[test]
    fn test_the_field_list_matches_the_decoder_stride() {
        let fields = EventType::Series
            .compact_fields()
            .expect("Series has a decoder");
        assert_eq!(fields.len(), 15, "the layout the server confirmed is 15");
        assert_eq!(fields.len(), series_row("AAPL", 21533).len());
    }

    #[test]
    fn test_a_real_row_decodes_field_for_field() {
        let events =
            try_parse_compact_data(&batch(series_row("AAPL", 21533))).expect("well formed");
        match &events[0] {
            MarketEvent::Series(series) => {
                assert_eq!(series.event_symbol, "AAPL");
                assert_eq!(series.event_time, 0);
                assert_eq!(series.event_flags, 4);
                assert_eq!(series.index, 23);
                assert_eq!(series.time, 1_785_542_361_974);
                assert_eq!(series.sequence, 0);
                // Days since the epoch, 2028-12-15. Not yyyymmdd.
                assert_eq!(series.expiration, 21533);
                assert_eq!(series.volatility, 0.3188);
                assert_eq!(series.call_volume, 8526.0);
                assert_eq!(series.put_volume, 1899.0);
                assert_eq!(series.put_call_ratio, 0.222_730_471_498_944_4);
                assert_eq!(series.forward_price, 335.174_395_879_884);
                assert_eq!(series.dividend, 0.0);
                assert_eq!(series.interest, 0.0);
            }
            other => panic!("expected a series, got {other:?}"),
        }
    }

    /// A subscription replays the whole chain in one batch, one row per
    /// expiration, which is how the real server sends it.
    #[test]
    fn test_a_whole_chain_decodes_expiration_by_expiration() {
        let mut values = series_row("AAPL", 21533);
        values.extend(series_row("AAPL", 21260));
        values.extend(series_row("AAPL", 21204));

        let events = try_parse_compact_data(&batch(values)).expect("well formed");
        assert_eq!(events.len(), 3);

        let expirations: Vec<i64> = events
            .iter()
            .map(|event| match event {
                MarketEvent::Series(series) => series.expiration,
                other => panic!("expected a series, got {other:?}"),
            })
            .collect();
        // The stride of fifteen is what keeps these distinct.
        assert_eq!(expirations, [21533, 21260, 21204]);
    }

    /// The point of issue #67: an undecodable type does not merely go missing,
    /// it abandons the rest of the batch it appears in.
    #[test]
    fn test_a_batch_mixing_series_and_quote_decodes_whole() {
        let mut data = batch(series_row("AAPL", 21533));
        data.push(CompactData::EventType("Quote".to_string()));
        data.push(CompactData::Values(vec![
            json!("Quote"),
            json!("AAPL"),
            json!(307.33),
            json!(307.35),
            json!(2160.0),
            json!(80.0),
        ]));

        let events = try_parse_compact_data(&data).expect("both types decode now");
        assert_eq!(
            events.len(),
            2,
            "a Series row used to abort the batch and take the Quote with it"
        );
        assert!(matches!(events[0], MarketEvent::Series(_)));
        assert!(matches!(events[1], MarketEvent::Quote(_)));
    }

    #[test]
    fn test_an_expiration_with_no_options_traded_decodes() {
        // No volume on either side leaves the ratio undefined, and the protocol
        // sends NaN rather than omitting the column.
        let mut values = series_row("AAPL", 21533);
        values[9] = json!(0.0);
        values[10] = json!(0.0);
        values[11] = json!("NaN");

        let events = try_parse_compact_data(&batch(values)).expect("NaN is a value");
        match &events[0] {
            MarketEvent::Series(series) => {
                assert_eq!(series.call_volume, 0.0);
                assert!(series.put_call_ratio.is_nan());
            }
            other => panic!("expected a series, got {other:?}"),
        }
    }

    #[test]
    fn test_a_fractional_expiration_is_rejected() {
        let mut values = series_row("AAPL", 21533);
        values[7] = json!(21533.5);

        let error =
            try_parse_compact_data(&batch(values)).expect_err("a day count is a whole number");
        assert!(
            error.to_string().contains("expiration"),
            "the field is missing: {error}"
        );
    }

    #[test]
    fn test_a_truncated_series_row_is_rejected() {
        let mut values = series_row("AAPL", 21533);
        values.pop();
        assert!(try_parse_compact_data(&batch(values)).is_err());
    }
}

#[cfg(test)]
mod series_snapshot_tests {
    use super::*;
    use serde_json::json;

    /// The row a real feed ends a Series snapshot with: flags set, everything
    /// else zeroed or NaN. Captured from the demo server, which closes a
    /// 24-row AAPL chain exactly like this.
    fn marker_row() -> Vec<Value> {
        vec![
            json!("Series"),
            json!("AAPL"),
            json!(0i64),
            json!(10i64), // eventFlags: the snapshot terminator
            json!(0i64),
            json!(1_785_542_361_974i64),
            json!(0i64),
            json!(0i64), // expiration: not an expiration at all
            json!("NaN"),
            json!("NaN"),
            json!("NaN"),
            json!("NaN"),
            json!("NaN"),
            json!("NaN"),
            json!("NaN"),
        ]
    }

    #[test]
    fn test_the_snapshot_marker_decodes_and_is_distinguishable() {
        let events = try_parse_compact_data(&[
            CompactData::EventType("Series".to_string()),
            CompactData::Values(marker_row()),
        ])
        .expect("the marker is a well formed row and must not error");

        match &events[0] {
            MarketEvent::Series(series) => {
                // It has to decode, because erroring here would abandon the
                // rest of the batch it closes.
                assert_eq!(series.expiration, 0);
                assert!(series.volatility.is_nan());
                // And a consumer has to be able to tell it apart from data.
                assert_ne!(
                    series.event_flags, 0,
                    "the marker is only distinguishable by its flags"
                );
            }
            other => panic!("expected a series, got {other:?}"),
        }
    }
}
