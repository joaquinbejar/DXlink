//! Smoke tests against the real dxFeed demo server.
//!
//! `#[ignore]`d because they need the network; run them with
//! `cargo test --test tests -- --ignored --nocapture`.
//!
//! **These exist to catch what the offline suite structurally cannot.** Every
//! other test in this directory drives a mock server written from our own
//! reading of the protocol, so a misunderstanding of the wire format is baked
//! into both sides of the exchange and passes. Only a real server can disagree
//! with us.
//!
//! The specific failure they are here for is COMPACT column drift, which does
//! not error: it puts plausible numbers in the wrong fields. So the assertions
//! are about the *shape* of what arrives — a price that is a price, a symbol
//! that is one of the symbols we asked for, a bar whose low is not above its
//! high — rather than about market activity, which nobody controls.
//!
//! What they do **not** assert is that any particular event type arrives. The
//! feed is delayed and demo entitlements vary, so requiring a `Trade` outside
//! market hours would make a green test mean "the market is open". Coverage of
//! what did arrive is printed instead, so a run is readable.

use dxlink::{DXLinkClient, EventType, FeedSubscription, MarketEvent};
use std::collections::BTreeSet;
use std::env;
use std::time::Duration;
use tokio::time::timeout;

/// dxFeed's own public demo, which serves delayed data and needs no token.
///
/// Note the `/market-data/` in the path. The shorter
/// `wss://demo.dxfeed.com/dxlink-ws` answers HTTP 400 on upgrade, and that once
/// got reported as a client bug.
const PUBLIC_DEMO_URL: &str = "wss://demo.dxfeed.com/market-data/dxlink-ws";

/// Where to point, and whether a token is needed.
///
/// Defaults to the public demo so these run with no credentials at all. Set
/// `DXLINK_WS_URL` to aim them somewhere else — tastytrade's delayed endpoint is
/// `wss://tasty-demo-dxlink-md-ws.dxfeed.com/delayed` and does require a token,
/// which comes from tastytrade's `/api-quote-tokens`.
fn endpoint() -> (String, String) {
    let url = env::var("DXLINK_WS_URL").unwrap_or_else(|_| PUBLIC_DEMO_URL.to_string());
    let token = env::var("DXLINK_API_TOKEN").unwrap_or_default();

    // Only the public demo is anonymous. Anywhere else, a missing token shows up
    // as UNAUTHORIZED from the server, which reads like a client bug rather than
    // a missing credential.
    assert!(
        url == PUBLIC_DEMO_URL || !token.trim().is_empty(),
        "DXLINK_WS_URL is set to {url} but DXLINK_API_TOKEN is empty. Only \
         {PUBLIC_DEMO_URL} accepts an anonymous session; anything else rejects \
         it at AUTH."
    );

    (url, token)
}

/// Liquid enough that something is usually moving.
const EQUITY: &str = "AAPL";
const OTHER_EQUITY: &str = "MSFT";
/// Five minute bars on the same underlying.
const CANDLE: &str = "AAPL{=5m}";

/// How long to sit and collect before deciding what arrived.
const COLLECT: Duration = Duration::from_secs(20);

/// Every type this client can decode, which is what the channel is configured
/// for: the server refusing one of these field lists is itself a finding.
const DECODED: &[EventType] = &[
    EventType::Quote,
    EventType::Trade,
    EventType::Greeks,
    EventType::Candle,
    EventType::Summary,
    EventType::TimeAndSale,
    EventType::Profile,
    EventType::Underlying,
    EventType::TheoPrice,
];

/// Turns on tracing when `RUST_LOG` asks for it.
///
/// Worth having on a network smoke specifically: when one of these fails the
/// first question is always whether the client sent the wrong thing or the
/// server answered oddly, and only the protocol log settles it.
fn trace_the_wire() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if env::var("RUST_LOG").is_ok() {
            // The outbound log redacts credentials before it prints, so this is
            // safe to enable with a real token in the environment.
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .try_init();
        }
    });
}

fn subscription(event_type: &str, symbol: &str) -> FeedSubscription {
    FeedSubscription {
        event_type: event_type.to_string(),
        symbol: symbol.to_string(),
        from_time: None,
        source: None,
    }
}

/// A number that could be a price. Catches a column holding a timestamp, a
/// count, or a value shifted in from a neighbouring field.
fn plausible_price(value: f64, field: &str, symbol: &str) {
    assert!(
        value.is_finite(),
        "{symbol}: {field} is {value}, which is not a number a price can be"
    );
    assert!(
        value > 0.0,
        "{symbol}: {field} is {value}; a traded instrument has no zero or \
         negative price, so this column is probably not the one we think"
    );
    assert!(
        value < 1_000_000.0,
        "{symbol}: {field} is {value}, which is more likely an epoch \
         millisecond that drifted into a price column"
    );
}

/// A number that could be a size or a volume: zero is legitimate, negative is
/// not, and `NaN` means "not applicable" rather than a failure.
fn plausible_size(value: f64, field: &str, symbol: &str) {
    if value.is_nan() {
        return;
    }
    assert!(
        value.is_finite(),
        "{symbol}: {field} is {value}, which is not a size"
    );
    assert!(value >= 0.0, "{symbol}: {field} is negative ({value})");
}

/// An epoch millisecond in the range a live feed can produce, or zero.
///
/// Catches a price that drifted into a time column, which is the same bug as
/// the other way round. Zero is allowed because the server genuinely sends it
/// for a timestamp it did not populate — a real `Summary` from the demo feed
/// arrives with `eventTime: 0`.
fn plausible_time(value: i64, field: &str, symbol: &str) {
    if value == 0 {
        return;
    }
    // 2001-09-09 to 2100-01-01. Wide on purpose: this is a shape check, not a
    // clock check, and a delayed feed replays older stamps.
    assert!(
        (1_000_000_000_000..4_102_444_800_000).contains(&value),
        "{symbol}: {field} is {value}, which is not an epoch millisecond"
    );
}

/// A day identifier: **days since the Unix epoch**, not `yyyymmdd`.
///
/// This assertion started life as a `yyyymmdd` range, which is the natural
/// guess and wrong. The real feed says 20665 for 2026-07-31, and that is
/// exactly the kind of thing only a real server tells you.
fn plausible_day_id(value: i64, field: &str, symbol: &str) {
    // 1997-05-19 to 2079-09-28. A yyyymmdd would be far outside this.
    assert!(
        (10_000..40_000).contains(&value),
        "{symbol}: {field} is {value}, which is not a day count since the epoch"
    );
}

/// Checks one event against the layout it claims, and reports its type.
///
/// This is the whole point of the file. Every field named here is read at a
/// fixed column offset, so a layout that disagrees with the server shows up as
/// a value that fails one of these.
fn validate(event: &MarketEvent) -> &'static str {
    match event {
        MarketEvent::Quote(quote) => {
            let symbol = &quote.event_symbol;
            plausible_price(quote.bid_price, "bidPrice", symbol);
            plausible_price(quote.ask_price, "askPrice", symbol);
            plausible_size(quote.bid_size, "bidSize", symbol);
            plausible_size(quote.ask_size, "askSize", symbol);
            "Quote"
        }
        MarketEvent::Trade(trade) => {
            let symbol = &trade.event_symbol;
            plausible_price(trade.price, "price", symbol);
            plausible_size(trade.size, "size", symbol);
            plausible_size(trade.day_volume, "dayVolume", symbol);
            "Trade"
        }
        MarketEvent::Greeks(greeks) => {
            let symbol = &greeks.event_symbol;
            // Delta is bounded by definition, which makes it the single best
            // column-drift detector in the protocol.
            assert!(
                greeks.delta.is_nan() || (-1.0..=1.0).contains(&greeks.delta),
                "{symbol}: delta is {}, which is outside what a delta can be",
                greeks.delta
            );
            assert!(
                greeks.volatility.is_nan() || greeks.volatility >= 0.0,
                "{symbol}: volatility is negative ({})",
                greeks.volatility
            );
            "Greeks"
        }
        MarketEvent::Candle(candle) => {
            let symbol = &candle.event_symbol;
            plausible_time(candle.time, "time", symbol);
            for (value, field) in [
                (candle.open, "open"),
                (candle.high, "high"),
                (candle.low, "low"),
                (candle.close, "close"),
            ] {
                plausible_price(value, field, symbol);
            }
            // The ordering is the assertion a shifted layout cannot satisfy.
            assert!(
                candle.high >= candle.low,
                "{symbol}: high {} is below low {}",
                candle.high,
                candle.low
            );
            assert!(
                candle.open <= candle.high && candle.open >= candle.low,
                "{symbol}: open {} is outside [{}, {}]",
                candle.open,
                candle.low,
                candle.high
            );
            assert!(
                candle.close <= candle.high && candle.close >= candle.low,
                "{symbol}: close {} is outside [{}, {}]",
                candle.close,
                candle.low,
                candle.high
            );
            plausible_size(candle.volume, "volume", symbol);
            "Candle"
        }
        MarketEvent::Summary(summary) => {
            let symbol = &summary.event_symbol;
            plausible_time(summary.event_time, "eventTime", symbol);
            // A text column sitting between numeric ones: a shift in either
            // direction lands a number here and fails the decode before this
            // line, so reaching it already proves something.
            assert!(
                summary
                    .day_close_price_type
                    .chars()
                    .all(|c| c.is_ascii_alphabetic()),
                "{symbol}: dayClosePriceType is {:?}, which is not a price type",
                summary.day_close_price_type
            );
            plausible_day_id(summary.day_id, "dayId", symbol);
            plausible_day_id(summary.prev_day_id, "prevDayId", symbol);
            "Summary"
        }
        MarketEvent::TimeAndSale(print) => {
            let symbol = &print.event_symbol;
            plausible_time(print.time, "time", symbol);
            plausible_price(print.price, "price", symbol);
            plausible_size(print.size, "size", symbol);
            assert!(
                print.exchange_code.len() <= 4,
                "{symbol}: exchangeCode is {:?}, so the text columns may be shifted",
                print.exchange_code
            );
            "TimeAndSale"
        }
        MarketEvent::Profile(profile) => {
            let symbol = &profile.event_symbol;
            assert!(
                !profile.description.is_empty(),
                "{symbol}: description is empty, so the text columns may be shifted"
            );
            assert!(
                profile.high_52_week_price.is_nan()
                    || profile.low_52_week_price.is_nan()
                    || profile.high_52_week_price >= profile.low_52_week_price,
                "{symbol}: 52 week high {} is below the low {}",
                profile.high_52_week_price,
                profile.low_52_week_price
            );
            "Profile"
        }
        MarketEvent::Underlying(surface) => {
            let symbol = &surface.event_symbol;
            for (value, field) in [
                (surface.volatility, "volatility"),
                (surface.front_volatility, "frontVolatility"),
                (surface.back_volatility, "backVolatility"),
            ] {
                assert!(
                    value.is_nan() || (0.0..10.0).contains(&value),
                    "{symbol}: {field} is {value}, which is not a volatility"
                );
            }
            plausible_size(surface.call_volume, "callVolume", symbol);
            plausible_size(surface.put_volume, "putVolume", symbol);
            "Underlying"
        }
        MarketEvent::TheoPrice(theo) => {
            let symbol = &theo.event_symbol;
            plausible_time(theo.time, "time", symbol);
            assert!(
                theo.delta.is_nan() || (-1.0..=1.0).contains(&theo.delta),
                "{symbol}: delta is {}",
                theo.delta
            );
            "TheoPrice"
        }
    }
}

/// Connects and configures the channel for every decodable type.
async fn open_session() -> (DXLinkClient, tokio::sync::mpsc::Receiver<MarketEvent>, u32) {
    trace_the_wire();

    // The token is never printed: it goes straight into the client.
    let (url, token) = endpoint();
    println!("--- connecting to {url}");

    let mut client = DXLinkClient::new(&url, &token);
    let stream = client
        .connect()
        .await
        .expect("failed to connect to the demo server");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("failed to create a feed channel");

    // Every type at once, deliberately. `setup_feed` validates the FEED_CONFIG
    // reply against what we asked for, so a server that disagrees with any of
    // these field lists fails here rather than silently later.
    client
        .setup_feed(channel_id, DECODED)
        .await
        .expect("the server rejected a field list this client requests");

    (client, stream, channel_id)
}

/// Collects for `COLLECT`, validating everything, and reports which types
/// arrived and how many events in total.
async fn collect_and_validate(
    stream: &mut tokio::sync::mpsc::Receiver<MarketEvent>,
) -> (BTreeSet<&'static str>, usize) {
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    let deadline = tokio::time::Instant::now() + COLLECT;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, stream.recv()).await {
            Ok(Some(event)) => {
                if total < 5 {
                    // A handful in full, so a failing run shows what the wire
                    // actually looked like and not just an assertion.
                    println!("sample: {event:?}");
                }
                seen.insert(validate(&event));
                total += 1;
            }
            Ok(None) => panic!("the stream closed mid-session, which is a session failure"),
            Err(_) => break,
        }
    }

    (seen, total)
}

/// The main smoke: everything this client can decode, against the real server,
/// with every arriving event checked for shape.
#[tokio::test]
#[ignore = "needs network access to the dxFeed demo server"]
async fn test_real_server_delivers_well_formed_events() {
    let (mut client, mut stream, channel_id) = open_session().await;

    let mut subscriptions = Vec::new();
    for event_type in [
        "Quote",
        "Trade",
        "Summary",
        "Profile",
        "TimeAndSale",
        "Underlying",
    ] {
        subscriptions.push(subscription(event_type, EQUITY));
    }
    // A second symbol, because a stride error usually shows up as the second
    // row of a batch carrying the first row's symbol.
    subscriptions.push(subscription("Quote", OTHER_EQUITY));
    subscriptions.push(subscription("Trade", OTHER_EQUITY));
    subscriptions.push(subscription("Candle", CANDLE));

    client
        .subscribe(channel_id, subscriptions)
        .await
        .expect("failed to subscribe");

    let (seen, total) = collect_and_validate(&mut stream).await;

    println!("--- decoded {total} event(s) in {COLLECT:?}");
    for event_type in DECODED {
        let name = event_type.to_string();
        let mark = if seen.contains(name.as_str()) {
            "yes"
        } else {
            "none"
        };
        println!("    {name:<12} {mark}");
    }

    // Not "every type arrived": entitlements and market hours decide that, and a
    // test that depends on them reports the wrong thing when it fails. What must
    // hold is that the session produced data and none of it was malformed.
    assert!(
        total > 0,
        "no events at all in {COLLECT:?}. Either the feed is closed, the token \
         has no entitlements, or the subscription never took"
    );
    assert!(
        client.disconnect_reason().is_none(),
        "the session died during collection: {:?}",
        client.disconnect_reason()
    );

    client.disconnect().await.expect("failed to disconnect");
}

/// Historical candles are the indexed path: a `fromTime` subscription replays a
/// snapshot, a different code path from a live update and the one most likely
/// to expose a wrong `eventFlags` or `index` column.
#[tokio::test]
#[ignore = "needs network access to the dxFeed demo server"]
async fn test_real_server_replays_historical_candles() {
    let (mut client, mut stream, channel_id) = open_session().await;

    // Fixed and well in the past, so there is something to replay whatever day
    // this runs on.
    let from_time = 1_700_000_000_000i64;

    client
        .subscribe(
            channel_id,
            vec![FeedSubscription {
                event_type: "Candle".to_string(),
                symbol: CANDLE.to_string(),
                from_time: Some(from_time),
                source: None,
            }],
        )
        .await
        .expect("failed to subscribe to historical candles");

    let mut bars = Vec::new();
    let deadline = tokio::time::Instant::now() + COLLECT;
    while bars.len() < 10 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, stream.recv()).await {
            Ok(Some(event)) => {
                validate(&event);
                if let MarketEvent::Candle(candle) = event {
                    bars.push(candle);
                }
            }
            Ok(None) => panic!("the stream closed during the replay"),
            Err(_) => break,
        }
    }

    println!("--- replayed {} bar(s)", bars.len());
    for bar in bars.iter().take(3) {
        println!(
            "    t={} o={} h={} l={} c={} v={} flags={} idx={}",
            bar.time,
            bar.open,
            bar.high,
            bar.low,
            bar.close,
            bar.volume,
            bar.event_flags,
            bar.index
        );
    }

    assert!(
        !bars.is_empty(),
        "no historical bars arrived for {CANDLE}. Known cause, issue #63: this \
         server negotiates a Candle layout without VWAP, the decoder is \
         compiled against one with it, so the channel is invalidated and every \
         bar is dropped. Run with RUST_LOG=dxlink=debug to see them arriving."
    );
    for bar in &bars {
        assert_eq!(
            bar.event_symbol, CANDLE,
            "a bar came back for another symbol"
        );
    }

    client.disconnect().await.expect("failed to disconnect");
}

/// The keepalive interval is derived from the server's own `SETUP`, and getting
/// it wrong means the server hangs up mid-session. Only a real server enforces
/// that deadline, so this sits idle across one and checks nothing died.
#[tokio::test]
#[ignore = "needs network access to the dxFeed demo server"]
async fn test_real_server_session_survives_an_idle_keepalive_cycle() {
    let (mut client, mut stream, channel_id) = open_session().await;

    client
        .subscribe(channel_id, vec![subscription("Quote", EQUITY)])
        .await
        .expect("failed to subscribe");

    // Longer than any interval this client derives, so at least one maintenance
    // beat has to have gone out and been accepted.
    let idle = Duration::from_secs(75);
    println!("--- sitting idle for {idle:?} to cross a keepalive cycle");

    let deadline = tokio::time::Instant::now() + idle;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, stream.recv()).await {
            // Drained rather than ignored: a full queue would drop events and
            // muddy what this is measuring.
            Ok(Some(event)) => {
                validate(&event);
            }
            Ok(None) => panic!(
                "the session died while idle, which is what a wrong keepalive \
                 looks like: {:?}",
                client.disconnect_reason()
            ),
            Err(_) => break,
        }
    }

    assert!(
        client.disconnect_reason().is_none(),
        "the session ended during the idle period: {:?}",
        client.disconnect_reason()
    );

    // And it is still usable afterwards, not merely un-dead.
    client
        .subscribe(channel_id, vec![subscription("Quote", OTHER_EQUITY)])
        .await
        .expect("the connection was not usable after sitting idle");

    client.disconnect().await.expect("failed to disconnect");
}
