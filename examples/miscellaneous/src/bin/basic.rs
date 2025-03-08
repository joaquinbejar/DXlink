use dxlink::{DXLinkClient, EventType, FeedSubscription, MarketEvent};
use std::error::Error;
use std::time::Duration;
use tokio::time::sleep;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Configure logging
    tracing_subscriber::fmt::init();

    info!("Starting DXLink client...");

    // DXFeed demo server
    let url = "wss://demo.dxfeed.com/dxlink-ws";
    let token = "";

    let mut client = DXLinkClient::new(url, token);

    info!("Connecting to DXLink server...");
    client.connect().await?;
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

    // Get stream for all events
    let mut event_stream = client.event_stream()?;

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
