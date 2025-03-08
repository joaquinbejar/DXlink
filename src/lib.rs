//! # dxlink
//!
//! `dxlink` is a Rust client library for the DXLink WebSocket protocol used by tastytrade
//! for real-time market data. This library provides a clean and type-safe API for connecting
//! to DXLink servers, subscribing to market events, and processing real-time market data.
//!
//! ## Features
//!
//! - Full implementation of the DXLink WebSocket protocol (AsyncAPI 2.4.0)
//! - Strongly typed event definitions for Quote, Trade, Greeks, and more
//! - Async/await based API for efficient resource usage
//! - Automatic handling of authentication and connection maintenance
//! - Support for multiple subscription channels
//! - Callback and stream-based APIs for event processing
//! - Robust error handling and reconnection logic
//!
//! ## Example
//!
//! Here's a basic example of using the library to connect to a DXLink server
//! and subscribe to market data:
//!
//! ```rust,no_run
//! use std::error::Error;
//! use dxlink::{DXLinkClient, EventType, FeedSubscription, MarketEvent};
//! use tokio::time::sleep;
//! use std::time::Duration;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn Error>> {
//!     
//!     // Create a new DXLink client with the API token
//!     // (typically obtained from tastytrade API)
//!     let token = "your_api_token_here";
//!     let url = "wss://tasty-openapi-ws.dxfeed.com/realtime";
//!     let mut client = DXLinkClient::new(url, token);
//!     
//!     // Connect to the DXLink server
//!     client.connect().await?;
//!     
//!     // Create a feed channel with AUTO contract type
//!     let channel_id = client.create_feed_channel("AUTO").await?;
//!     
//!     // Configure the channel for Quote and Trade events
//!     client.setup_feed(channel_id, &[EventType::Quote, EventType::Trade]).await?;
//!     
//!     // Register a callback for specific symbol
//!     client.on_event("SPY", |event| {
//!         println!("Event received for SPY: {:?}", event);
//!     });
//!     
//!     // Get a stream for all events
//!     let mut event_stream = client.event_stream()?;
//!     
//!     // Process events in a separate task
//!     tokio::spawn(async move {
//!         while let Some(event) = event_stream.recv().await {
//!             match &event {
//!                 MarketEvent::Quote(quote) => {
//!                     println!(
//!                         "Quote: {} - Bid: {} x {}, Ask: {} x {}",
//!                         quote.event_symbol,
//!                         quote.bid_price,
//!                         quote.bid_size,
//!                         quote.ask_price,
//!                         quote.ask_size
//!                     );
//!                 },
//!                 MarketEvent::Trade(trade) => {
//!                     println!(
//!                         "Trade: {} - Price: {}, Size: {}, Volume: {}",
//!                         trade.event_symbol,
//!                         trade.price,
//!                         trade.size,
//!                         trade.day_volume
//!                     );
//!                 },
//!                 _ => println!("Other event type: {:?}", event),
//!             }
//!         }
//!     });
//!     
//!     // Subscribe to some symbols
//!     let subscriptions = vec![
//!         FeedSubscription {
//!             event_type: "Quote".to_string(),
//!             symbol: "SPY".to_string(),
//!             from_time: None,
//!             source: None,
//!         },
//!         FeedSubscription {
//!             event_type: "Trade".to_string(),
//!             symbol: "SPY".to_string(),
//!             from_time: None,
//!             source: None,
//!         },
//!     ];
//!     
//!     client.subscribe(channel_id, subscriptions).await?;
//!     
//!     // Keep the connection active for some time
//!     sleep(Duration::from_secs(60)).await;
//!     
//!     // Cleanup
//!     client.disconnect().await?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Working with historical data
//!
//! DXLink supports subscribing to historical data through Candle events.
//! When subscribing to candle events, you need to specify the period,
//! type, and a timestamp from which to fetch the data:
//!
//! ```rust,no_run
//! use dxlink::FeedSubscription;
//! use std::time::{SystemTime, UNIX_EPOCH};
//!
//! // Get current timestamp in milliseconds
//! let now = SystemTime::now()
//!     .duration_since(UNIX_EPOCH)
//!     .unwrap()
//!     .as_millis() as i64;
//!
//! // Timestamp for 24 hours ago
//! let one_day_ago = now - (24 * 60 * 60 * 1000);
//!
//! // Subscribe to 5-minute candles for SPY for the last 24 hours
//! let candle_subscription = FeedSubscription {
//!     event_type: "Candle".to_string(),
//!     symbol: "SPY{=5m}".to_string(),  // 5-minute candles
//!     from_time: Some(one_day_ago),
//!     source: None,
//! };
//! ```
//!
//! ## Error Handling
//!
//! The library uses a custom error type `DXLinkError` that encompasses
//! various error cases that can occur when interacting with the DXLink API:
//!
//! ```rust,no_run
//! use dxlink::{DXLinkClient, DXLinkError};
//!
//! async fn example_error_handling() {
//!     let mut client = DXLinkClient::new("wss://example.com", "token");
//!     match client.connect().await {
//!         Ok(_) => println!("Connected successfully!"),
//!         Err(DXLinkError::Authentication(e)) => eprintln!("Authentication failed: {}", e),
//!         Err(DXLinkError::Connection(e)) => eprintln!("Connection error: {}", e),
//!         Err(e) => eprintln!("Other error: {}", e),
//!     }
//! }
//! ```
//!
//! ## Available Event Types
//!
//! The library supports the following event types:
//!
//! - `Quote` - Current bid/ask prices and sizes
//! - `Trade` - Last trade information
//! - `Greeks` - Option greeks data (delta, gamma, theta, etc.)
//! - `Summary` - Daily summary information
//! - `Profile` - Instrument profile information
//! - `Candle` - OHLC (Open, High, Low, Close) data for time periods
//! - And more!
//!
//!
//! ## License
//!
//! This project is licensed under the MIT License. See the LICENSE file for details.
//!
//!  ## Setup Instructions
//!  
//!  1. Clone the repository:
//!  ```shell
//!  git clone https://github.com/joaquinbejar/DXlink
//!  cd DXlink
//!  ```
//!  
//!  2. Build the project:
//!  ```shell
//!  make build
//!  ```
//!  
//!  3. Run tests:
//!  ```shell
//!  make test
//!  ```
//!  
//!  4. Format the code:
//!  ```shell
//!  make fmt
//!  ```
//!  
//!  5. Run linting:
//!  ```shell
//!  make lint
//!  ```
//!  
//!  6. Clean the project:
//!  ```shell
//!  make clean
//!  ```
//!  
//!  7. Run the project:
//!  ```shell
//!  make run
//!  ```
//!  
//!  8. Fix issues:
//!  ```shell
//!  make fix
//!  ```
//!  
//!  9. Run pre-push checks:
//!  ```shell
//!  make pre-push
//!  ```
//!  
//!  10. Generate documentation:
//!  ```shell
//!  make doc
//!  ```
//!  
//!  11. Publish the package:
//!  ```shell
//!  make publish
//!  ```
//!  
//!  12. Generate coverage report:
//!  ```shell
//!  make coverage
//!  ```
//!
//!
//!  ## Testing
//!  
//!  To run unit tests:
//!  ```shell
//!  make test
//!  ```
//!  
//!  To run tests with coverage:
//!  ```shell
//!  make coverage
//!  ```
//!  
//!  ## Contribution and Contact
//!  
//!  We welcome contributions to this project! If you would like to contribute, please follow these steps:
//!  
//!  1. Fork the repository.
//!  2. Create a new branch for your feature or bug fix.
//!  3. Make your changes and ensure that the project still builds and all tests pass.
//!  4. Commit your changes and push your branch to your forked repository.
//!  5. Submit a pull request to the main repository.
//!  
//!  If you have any questions, issues, or would like to provide feedback, please feel free to contact the project maintainer:
//!  
//!  **Joaquín Béjar García**
//!  - Email: jb@taunais.com
//!  - GitHub: [joaquinbejar](https://github.com/joaquinbejar)
//!  
//!  We appreciate your interest and look forward to your contributions!
//!  

pub mod client;
pub mod connection;
pub mod error;
pub mod events;
pub mod messages;
mod utils;

pub use client::DXLinkClient;
pub use error::DXLinkError;
pub use events::{EventType, MarketEvent};
pub use messages::FeedSubscription;
pub use utils::parse_compact_data;
