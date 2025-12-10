use dxlink::{DXLinkClient, EventType, FeedSubscription, MarketEvent};
use std::env;
use std::error::Error;
use std::time::Duration;
use tokio::time::sleep;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Configure logging
    tracing_subscriber::fmt::init();

    info!("Starting DXLink client...");

    // Get configuration from environment variables
    let url =
        env::var("DXLINK_WS_URL").unwrap_or_else(|_| "wss://demo.dxfeed.com/dxlink-ws".to_string());
    let token = env::var("DXLINK_API_TOKEN").unwrap_or_else(|_| String::new());

    info!("Using DXLink URL: {}", url);
    if token.is_empty() {
        info!("No API token provided - using demo mode");
    }

    let mut client = DXLinkClient::new(&url, &token);

    info!("Connecting to DXLink server...");
    let mut event_stream = client.connect().await?;
    info!("Connection successful!");

    // Create channel for feed
    info!("Creating channel...");
    let channel_id = client.create_feed_channel("AUTO").await?;
    info!("Channel created: {}", channel_id);

    // Setup channel - IMPORTANT: Include all event fields you need
    info!("Setting up channel...");
    client
        .setup_feed(channel_id, &[EventType::Quote, EventType::Trade])
        .await?;
    info!("Channel setup complete");

    // Register callback for events
    client.on_event("AAPL", |event| {
        info!("AAPL event received: {:?}", event);
    });

    // Process events in a separate task
    tokio::spawn(async move {
        info!("Waiting for events...");
        while let Some(event) = event_stream.recv().await {
            match &event {
                MarketEvent::Quote(quote) => {
                    info!(
                        "Quote: {} - Bid: {} x {}, Ask: {} x {}",
                        quote.event_symbol,
                        quote.bid_price,
                        quote.bid_size,
                        quote.ask_price,
                        quote.ask_size
                    );
                }
                MarketEvent::Trade(trade) => {
                    info!(
                        "Trade: {} - Price: {}, Size: {}, Volume: {}",
                        trade.event_symbol, trade.price, trade.size, trade.day_volume
                    );
                }
                _ => info!("Other event type: {:?}", event),
            }
        }
    });

    // Subscribe to symbols
    info!("Subscribing to symbols...");
    let subscriptions = vec![
        FeedSubscription {
            event_type: "Quote".to_string(),
            symbol: "AAPL".to_string(),
            from_time: None,
            source: None,
        },
        FeedSubscription {
            event_type: "Trade".to_string(),
            symbol: "AAPL".to_string(),
            from_time: None,
            source: None,
        },
        // Additional popular symbols
        FeedSubscription {
            event_type: "Quote".to_string(),
            symbol: "MSFT".to_string(),
            from_time: None,
            source: None,
        },
        FeedSubscription {
            event_type: "Quote".to_string(),
            symbol: "BTC/USD:GDAX".to_string(),
            from_time: None,
            source: None,
        },
    ];

    client.subscribe(channel_id, subscriptions).await?;
    info!("Subscription successful");

    // Keep connection active for 2 minutes
    info!("Receiving data for 2 minutes...");
    sleep(Duration::from_secs(120)).await;

    // Cleanup
    info!("Disconnecting...");
    client.disconnect().await?;
    info!("Disconnection successful");

    Ok(())
}
