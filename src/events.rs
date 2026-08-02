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
    /// This is what `setup_feed` requests, what the `FEED_CONFIG` reply is
    /// validated against, and what drives the decoder's stride in `utils.rs` —
    /// one definition, so the request and the layout being read cannot drift
    /// apart.
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
            EventType::Summary => Some(&[
                "eventType",
                "eventSymbol",
                "eventTime",
                "dayId",
                "dayOpenPrice",
                "dayHighPrice",
                "dayLowPrice",
                "dayClosePrice",
                "dayClosePriceType",
                "prevDayId",
                "prevDayClosePrice",
                "prevDayClosePriceType",
                "prevDayVolume",
                "openInterest",
            ]),
            EventType::Candle => Some(&[
                "eventType",
                "eventSymbol",
                "eventTime",
                "eventFlags",
                "index",
                "time",
                "sequence",
                "count",
                "open",
                "high",
                "low",
                "close",
                "volume",
                "VWAP",
                "bidVolume",
                "askVolume",
                "impVolatility",
                "openInterest",
            ]),
            EventType::Profile => Some(&[
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
            ]),
            EventType::TimeAndSale => Some(&[
                "eventType",
                "eventSymbol",
                "eventTime",
                "eventFlags",
                "index",
                "time",
                "timeNanoPart",
                "sequence",
                "exchangeCode",
                "price",
                "size",
                "bidPrice",
                "askPrice",
                "exchangeSaleConditions",
                "tradeThroughExempt",
                "aggressorSide",
                "spreadLeg",
                "extendedTradingHours",
                "validTick",
                "type",
                "buyer",
                "seller",
            ]),
            EventType::Underlying => Some(&[
                "eventType",
                "eventSymbol",
                "eventTime",
                "eventFlags",
                "index",
                "time",
                "sequence",
                "volatility",
                "frontVolatility",
                "backVolatility",
                "callVolume",
                "putVolume",
                "putCallRatio",
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
            EventType::Order
            | EventType::TradeETH
            | EventType::SpreadOrder
            | EventType::TheoPrice
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
/// bars. Every field the dxFeed AsyncAPI schema defines for a candle is here,
/// in the order the client requests them: `eventFlags` and `index` in
/// particular mark where a historical snapshot starts and ends, and a consumer
/// that only ever sees OHLC cannot tell a snapshot from live updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandleEvent {
    /// The type of the event, `Candle`.
    #[serde(rename = "eventType")]
    pub event_type: String,

    /// The candle symbol, including its period, such as `AAPL{=5m}`.
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,

    /// When the server emitted the event, as epoch milliseconds.
    #[serde(rename = "eventTime")]
    pub event_time: i64,

    /// Snapshot and transaction bits. Non-zero values delimit a historical
    /// snapshot; see the dxFeed event flags documentation.
    #[serde(rename = "eventFlags")]
    pub event_flags: i64,

    /// Unique index of the bar within its subscription.
    #[serde(rename = "index")]
    pub index: i64,

    /// Start of the bar, as epoch milliseconds.
    #[serde(rename = "time")]
    pub time: i64,

    /// Sequence number, disambiguating bars sharing a timestamp.
    #[serde(rename = "sequence")]
    pub sequence: i64,

    /// Number of events aggregated into the bar.
    #[serde(rename = "count")]
    pub count: i64,

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

    /// Volume weighted average price for the bar.
    #[serde(rename = "VWAP", with = "json_double")]
    pub vwap: f64,

    /// Volume traded at the bid during the bar.
    #[serde(rename = "bidVolume", with = "json_double")]
    pub bid_volume: f64,

    /// Volume traded at the ask during the bar.
    #[serde(rename = "askVolume", with = "json_double")]
    pub ask_volume: f64,

    /// Implied volatility over the bar, for instruments that have it.
    #[serde(rename = "impVolatility", with = "json_double")]
    pub imp_volatility: f64,

    /// Open interest at the end of the bar.
    #[serde(rename = "openInterest", with = "json_double")]
    pub open_interest: f64,
}

/// The session's opening, extremes and closes for an instrument.
///
/// Every field the dxFeed AsyncAPI schema defines for a summary, in the order
/// the client requests them. The `...PriceType` columns say whether a close is
/// final, indicative or preliminary, which is what stops a consumer treating a
/// provisional close as settled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryEvent {
    /// The type of the event, `Summary`.
    #[serde(rename = "eventType")]
    pub event_type: String,

    /// The symbol the summary relates to.
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,

    /// When the server emitted the event, as epoch milliseconds.
    #[serde(rename = "eventTime")]
    pub event_time: i64,

    /// Identifier of the current trading day.
    #[serde(rename = "dayId")]
    pub day_id: i64,

    /// First price of the current trading day.
    #[serde(rename = "dayOpenPrice", with = "json_double")]
    pub day_open_price: f64,

    /// Highest price of the current trading day.
    #[serde(rename = "dayHighPrice", with = "json_double")]
    pub day_high_price: f64,

    /// Lowest price of the current trading day.
    #[serde(rename = "dayLowPrice", with = "json_double")]
    pub day_low_price: f64,

    /// Closing price of the current trading day so far.
    #[serde(rename = "dayClosePrice", with = "json_double")]
    pub day_close_price: f64,

    /// Whether the day's close is final, indicative or preliminary.
    #[serde(rename = "dayClosePriceType")]
    pub day_close_price_type: String,

    /// Identifier of the previous trading day.
    #[serde(rename = "prevDayId")]
    pub prev_day_id: i64,

    /// Closing price of the previous trading day.
    #[serde(rename = "prevDayClosePrice", with = "json_double")]
    pub prev_day_close_price: f64,

    /// Whether the previous day's close is final, indicative or preliminary.
    #[serde(rename = "prevDayClosePriceType")]
    pub prev_day_close_price_type: String,

    /// Total volume of the previous trading day.
    #[serde(rename = "prevDayVolume", with = "json_double")]
    pub prev_day_volume: f64,

    /// Open interest, for instruments that have it.
    #[serde(rename = "openInterest", with = "json_double")]
    pub open_interest: f64,
}

/// Instrument metadata: what it is, whether it is tradable right now, and the
/// fundamentals that frame a price.
///
/// Every field the dxFeed AsyncAPI schema defines for a profile, in the order
/// the client requests them. `trading_status` and the halt window are the
/// operational half — a price from a halted instrument is stale by definition —
/// and the limit prices bound what the venue will accept at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEvent {
    /// The type of the event, `Profile`.
    #[serde(rename = "eventType")]
    pub event_type: String,

    /// The symbol the profile describes.
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,

    /// When the server emitted the event, as epoch milliseconds.
    #[serde(rename = "eventTime")]
    pub event_time: i64,

    /// Human-readable description of the instrument.
    pub description: String,

    /// Short sale restriction state: `Active`, `Inactive` or `Undefined`.
    #[serde(rename = "shortSaleRestriction")]
    pub short_sale_restriction: String,

    /// Whether trading is `Active`, `Halted` or `Undefined`.
    #[serde(rename = "tradingStatus")]
    pub trading_status: String,

    /// Why trading is in its current status, when the venue gives a reason.
    #[serde(rename = "statusReason")]
    pub status_reason: String,

    /// Start of the trading halt, as epoch milliseconds.
    #[serde(rename = "haltStartTime")]
    pub halt_start_time: i64,

    /// End of the trading halt, as epoch milliseconds.
    #[serde(rename = "haltEndTime")]
    pub halt_end_time: i64,

    /// Highest price the venue will accept today.
    #[serde(rename = "highLimitPrice", with = "json_double")]
    pub high_limit_price: f64,

    /// Lowest price the venue will accept today.
    #[serde(rename = "lowLimitPrice", with = "json_double")]
    pub low_limit_price: f64,

    /// Highest price over the last 52 weeks.
    #[serde(rename = "high52WeekPrice", with = "json_double")]
    pub high_52_week_price: f64,

    /// Lowest price over the last 52 weeks.
    #[serde(rename = "low52WeekPrice", with = "json_double")]
    pub low_52_week_price: f64,

    /// Beta against the market.
    #[serde(with = "json_double")]
    pub beta: f64,

    /// Earnings per share.
    #[serde(rename = "earningsPerShare", with = "json_double")]
    pub earnings_per_share: f64,

    /// Dividend payments per year.
    #[serde(rename = "dividendFrequency", with = "json_double")]
    pub dividend_frequency: f64,

    /// Amount of the last dividend that went ex.
    #[serde(rename = "exDividendAmount", with = "json_double")]
    pub ex_dividend_amount: f64,

    /// Day the last dividend went ex, as a day identifier.
    #[serde(rename = "exDividendDayId")]
    pub ex_dividend_day_id: i64,

    /// Shares outstanding.
    #[serde(with = "json_double")]
    pub shares: f64,

    /// Shares available to trade.
    #[serde(rename = "freeFloat", with = "json_double")]
    pub free_float: f64,
}

/// One execution as it printed, with the quote that stood around it.
///
/// Every field the dxFeed AsyncAPI schema defines for a trade print, in the
/// order the client requests them. The metadata is the reason to use this
/// instead of `Trade`: `exchange_sale_conditions` and `sale_type` say whether a
/// print is eligible for the consolidated tape or is a correction of an earlier
/// one, and `valid_tick` says whether it should move the last price at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeAndSaleEvent {
    /// The type of the event, `TimeAndSale`.
    #[serde(rename = "eventType")]
    pub event_type: String,

    /// The symbol that traded.
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,

    /// When the server emitted the event, as epoch milliseconds.
    #[serde(rename = "eventTime")]
    pub event_time: i64,

    /// Transactional and snapshot bits for this print.
    ///
    /// Time and sale is an indexed event, so a subscription with `fromTime`
    /// replays history as a snapshot delimited by these flags.
    #[serde(rename = "eventFlags")]
    pub event_flags: i64,

    /// Unique index of the print, ordering it within the stream.
    pub index: i64,

    /// When the print happened, as epoch milliseconds.
    pub time: i64,

    /// Sub-millisecond part of `time`, in nanoseconds.
    #[serde(rename = "timeNanoPart")]
    pub time_nano_part: i64,

    /// Sequence number, separating prints that share a millisecond.
    pub sequence: i64,

    /// Exchange the print came from, as its single-character code.
    #[serde(rename = "exchangeCode")]
    pub exchange_code: String,

    /// Execution price.
    #[serde(with = "json_double")]
    pub price: f64,

    /// Executed size.
    #[serde(with = "json_double")]
    pub size: f64,

    /// Bid at the time of the print, `NaN` when it was not known.
    #[serde(rename = "bidPrice", with = "json_double")]
    pub bid_price: f64,

    /// Ask at the time of the print, `NaN` when it was not known.
    #[serde(rename = "askPrice", with = "json_double")]
    pub ask_price: f64,

    /// Sale conditions the exchange reported, as their raw codes.
    #[serde(rename = "exchangeSaleConditions")]
    pub exchange_sale_conditions: String,

    /// Trade-through exempt flag, as the single-character regulatory code.
    #[serde(rename = "tradeThroughExempt")]
    pub trade_through_exempt: String,

    /// Which side initiated the trade: `Buy`, `Sell` or `Undefined`.
    #[serde(rename = "aggressorSide")]
    pub aggressor_side: String,

    /// Whether the print is one leg of a spread.
    #[serde(rename = "spreadLeg")]
    pub spread_leg: bool,

    /// Whether the print happened outside regular trading hours.
    #[serde(rename = "extendedTradingHours")]
    pub extended_trading_hours: bool,

    /// Whether the print is eligible to update the last price.
    #[serde(rename = "validTick")]
    pub valid_tick: bool,

    /// Whether this is a new print, a correction, or a cancellation.
    ///
    /// Named `sale_type` because the wire name `type` is a Rust keyword; the
    /// serialized field is still `type`.
    #[serde(rename = "type")]
    pub sale_type: String,

    /// Buying party, when the venue discloses it.
    pub buyer: String,

    /// Selling party, when the venue discloses it.
    pub seller: String,
}

/// The option surface over an underlying: implied volatility and the call/put
/// balance.
///
/// Every field the dxFeed AsyncAPI schema defines for an underlying, in the
/// order the client requests them. `front_volatility` and `back_volatility`
/// bracket the term structure, and `put_call_ratio` is the positioning number
/// the raw volumes are usually reduced to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnderlyingEvent {
    /// The type of the event, `Underlying`.
    #[serde(rename = "eventType")]
    pub event_type: String,

    /// The underlying symbol.
    #[serde(rename = "eventSymbol")]
    pub event_symbol: String,

    /// When the server emitted the event, as epoch milliseconds.
    #[serde(rename = "eventTime")]
    pub event_time: i64,

    /// Transactional and snapshot bits for this record.
    #[serde(rename = "eventFlags")]
    pub event_flags: i64,

    /// Unique index of the record, ordering it within the stream.
    pub index: i64,

    /// When the values were computed, as epoch milliseconds.
    pub time: i64,

    /// Sequence number, separating records that share a millisecond.
    pub sequence: i64,

    /// 30-day implied volatility for this underlying, as a fraction.
    #[serde(with = "json_double")]
    pub volatility: f64,

    /// Implied volatility of the front-month options.
    #[serde(rename = "frontVolatility", with = "json_double")]
    pub front_volatility: f64,

    /// Implied volatility of the second-month options.
    #[serde(rename = "backVolatility", with = "json_double")]
    pub back_volatility: f64,

    /// Call option volume for the day.
    #[serde(rename = "callVolume", with = "json_double")]
    pub call_volume: f64,

    /// Put option volume for the day.
    #[serde(rename = "putVolume", with = "json_double")]
    pub put_volume: f64,

    /// Put volume over call volume.
    #[serde(rename = "putCallRatio", with = "json_double")]
    pub put_call_ratio: f64,
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
    /// A daily summary: open, extremes and the previous close.
    Summary(SummaryEvent),
    /// One OHLC bar, from a historical or streaming candle subscription.
    Candle(CandleEvent),
    /// One execution as it printed, with the surrounding quote.
    TimeAndSale(TimeAndSaleEvent),
    /// Instrument metadata: description, trading status and fundamentals.
    Profile(ProfileEvent),
    /// The option surface over an underlying: implied volatility and volumes.
    ///
    /// New variants go last on purpose: `MarketEvent` is `#[serde(untagged)]`,
    /// so serde tries them in declaration order and keeps the first that
    /// deserializes. Appending leaves every variant already in the list
    /// matching exactly as it did. Each type has a round-trip test that would
    /// catch one variant stealing another's payload.
    Underlying(UnderlyingEvent),
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

/// Serde coverage for the event types added after the original three.
///
/// The property is stronger than "the names look right": each struct's
/// serialized key set has to equal that type's [`EventType::compact_fields`]
/// list, which is the same list `setup_feed` sends. A `#[serde(rename)]` that
/// drifts from the wire name fails here rather than on a live feed.
#[cfg(test)]
mod event_serde_tests {
    use super::*;
    use serde_json::{Value, from_str, to_string};

    /// Asserts the serialized field names are exactly the layout, no more and
    /// no less, and that the value survives a round trip.
    fn assert_wire_contract<T>(event: &T, event_type: EventType)
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let serialized = to_string(event).expect("serialize");
        let value: Value = from_str(&serialized).expect("valid JSON");
        let object = value.as_object().expect("an event is an object");

        let mut got: Vec<&str> = object.keys().map(String::as_str).collect();
        got.sort_unstable();

        let mut want: Vec<&str> = event_type
            .compact_fields()
            .expect("the type has a layout")
            .to_vec();
        want.sort_unstable();

        assert_eq!(
            got, want,
            "{event_type}'s serde names do not match the fields it requests"
        );

        from_str::<T>(&serialized).expect("round trip");
    }

    fn candle() -> CandleEvent {
        CandleEvent {
            event_type: "Candle".to_string(),
            event_symbol: "AAPL{=5m}".to_string(),
            event_time: 1_700_000_000_500,
            event_flags: 0,
            index: 7,
            time: 1_700_000_000_000,
            sequence: 3,
            count: 42,
            open: 149.0,
            high: 151.0,
            low: 148.5,
            close: 150.5,
            volume: 1_234_000.0,
            vwap: 150.1,
            bid_volume: 600_000.0,
            ask_volume: 634_000.0,
            imp_volatility: 0.31,
            open_interest: 4_200.0,
        }
    }

    fn summary() -> SummaryEvent {
        SummaryEvent {
            event_type: "Summary".to_string(),
            event_symbol: "AAPL".to_string(),
            event_time: 1_700_000_000_500,
            day_id: 20240119,
            day_open_price: 149.5,
            day_high_price: 152.0,
            day_low_price: 148.0,
            day_close_price: 150.75,
            day_close_price_type: "Final".to_string(),
            prev_day_id: 20240118,
            prev_day_close_price: 147.75,
            prev_day_close_price_type: "Final".to_string(),
            prev_day_volume: 58_000_000.0,
            open_interest: 4_200.0,
        }
    }

    fn print() -> TimeAndSaleEvent {
        TimeAndSaleEvent {
            event_type: "TimeAndSale".to_string(),
            event_symbol: "AAPL".to_string(),
            event_time: 1_700_000_000_500,
            event_flags: 0,
            index: 11,
            time: 1_700_000_000_000,
            time_nano_part: 250_000,
            sequence: 3,
            exchange_code: "Q".to_string(),
            price: 151.25,
            size: 75.0,
            bid_price: 151.2,
            ask_price: 151.3,
            exchange_sale_conditions: "@ TI".to_string(),
            trade_through_exempt: "X".to_string(),
            aggressor_side: "Buy".to_string(),
            spread_leg: false,
            extended_trading_hours: true,
            valid_tick: true,
            sale_type: "NEW".to_string(),
            buyer: "NSDQ".to_string(),
            seller: "NYSE".to_string(),
        }
    }

    fn profile() -> ProfileEvent {
        ProfileEvent {
            event_type: "Profile".to_string(),
            event_symbol: "AAPL".to_string(),
            event_time: 1_700_000_000_500,
            description: "Apple Inc. - Common Stock".to_string(),
            short_sale_restriction: "Inactive".to_string(),
            trading_status: "Halted".to_string(),
            status_reason: "News pending".to_string(),
            halt_start_time: 1_700_000_100_000,
            halt_end_time: 1_700_000_900_000,
            high_limit_price: 165.0,
            low_limit_price: 135.0,
            high_52_week_price: 199.62,
            low_52_week_price: 124.17,
            beta: 1.29,
            earnings_per_share: 6.13,
            dividend_frequency: 4.0,
            ex_dividend_amount: 0.24,
            ex_dividend_day_id: 20240209,
            shares: 15_552_800_000.0,
            free_float: 15_461_900_000.0,
        }
    }

    fn underlying() -> UnderlyingEvent {
        UnderlyingEvent {
            event_type: "Underlying".to_string(),
            event_symbol: "SPX".to_string(),
            event_time: 1_700_000_000_500,
            event_flags: 0,
            index: 9,
            time: 1_700_000_000_000,
            sequence: 3,
            volatility: 0.25,
            front_volatility: 0.28,
            back_volatility: 0.22,
            call_volume: 310_000.0,
            put_volume: 465_000.0,
            put_call_ratio: 1.5,
        }
    }

    #[test]
    fn test_underlying_serde_names_match_its_layout() {
        assert_wire_contract(&underlying(), EventType::Underlying);
    }

    #[test]
    fn test_candle_serde_names_match_its_layout() {
        assert_wire_contract(&candle(), EventType::Candle);
    }

    #[test]
    fn test_summary_serde_names_match_its_layout() {
        assert_wire_contract(&summary(), EventType::Summary);
    }

    #[test]
    fn test_time_and_sale_serde_names_match_its_layout() {
        assert_wire_contract(&print(), EventType::TimeAndSale);
    }

    #[test]
    fn test_profile_serde_names_match_its_layout() {
        assert_wire_contract(&profile(), EventType::Profile);
    }

    /// A FULL-format payload carries the non-finite doubles as strings, so the
    /// structs have to read them back the same way the COMPACT decoder does.
    #[test]
    fn test_non_finite_doubles_survive_a_full_round_trip() {
        let mut bar = candle();
        bar.imp_volatility = f64::NAN;
        let serialized = to_string(&bar).expect("serialize");
        assert!(
            serialized.contains("\"impVolatility\":\"NaN\""),
            "NaN is not a JSON literal, it has to be the string: {serialized}"
        );
        let back: CandleEvent = from_str(&serialized).expect("round trip");
        assert!(back.imp_volatility.is_nan());

        let mut instrument = profile();
        instrument.high_limit_price = f64::INFINITY;
        instrument.shares = f64::NAN;
        let serialized = to_string(&instrument).expect("serialize");
        assert!(
            serialized.contains("\"highLimitPrice\":\"Infinity\""),
            "wrong encoding: {serialized}"
        );
        let back: ProfileEvent = from_str(&serialized).expect("round trip");
        assert_eq!(back.high_limit_price, f64::INFINITY);
        assert!(back.shares.is_nan());
    }

    /// Untagged deserialization keeps the first variant that fits, so each new
    /// type has to still come back as itself.
    #[test]
    fn test_each_new_type_round_trips_as_its_own_variant() {
        for event in [
            MarketEvent::Candle(candle()),
            MarketEvent::Summary(summary()),
            MarketEvent::TimeAndSale(print()),
            MarketEvent::Profile(profile()),
            MarketEvent::Underlying(underlying()),
        ] {
            let serialized = to_string(&event).expect("serialize");
            let back: MarketEvent = from_str(&serialized).expect("round trip");
            assert_eq!(
                std::mem::discriminant(&event),
                std::mem::discriminant(&back),
                "an untagged round trip picked the wrong variant for {serialized}"
            );
        }
    }
}
