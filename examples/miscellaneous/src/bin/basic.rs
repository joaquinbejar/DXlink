use dxlink::{DXLinkClient, EventType, FeedSubscription, MarketEvent};
use std::error::Error;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Configurar logging
    tracing_subscriber::fmt::init();

    // Obtener token (normalmente lo obtendrías de la API de tastytrade)
    let token = "tu_token_api_aquí";
    let url = "wss://tasty-openapi-ws.dxfeed.com/realtime";

    // Crear cliente DXLink
    let mut client = DXLinkClient::new(url, token);

    // Conectar y autenticar
    println!("Conectando a DXLink...");
    client.connect().await?;
    println!("Conexión exitosa!");

    // Crear canal de feed
    println!("Creando canal...");
    let channel_id = client.create_feed_channel("AUTO").await?;
    println!("Canal creado: {}", channel_id);

    // Configurar canal
    println!("Configurando canal...");
    client
        .setup_feed(channel_id, &[EventType::Quote, EventType::Trade])
        .await?;
    println!("Canal configurado");

    // Registrar callback para SPY
    client.on_event("SPY", |event| {
        println!("Evento recibido para SPY: {:?}", event);
    });

    // Obtener stream para todos los eventos
    let mut event_stream = client.event_stream()?;

    // Spawn task para procesar eventos
    tokio::spawn(async move {
        while let Some(event) = event_stream.recv().await {
            match &event {
                MarketEvent::Quote(quote) => {
                    println!(
                        "Quote: {} - Bid: {} x {}, Ask: {} x {}",
                        quote.event_symbol,
                        quote.bid_price,
                        quote.bid_size,
                        quote.ask_price,
                        quote.ask_size
                    );
                }
                MarketEvent::Trade(trade) => {
                    println!(
                        "Trade: {} - Price: {}, Size: {}, Volume: {}",
                        trade.event_symbol, trade.price, trade.size, trade.day_volume
                    );
                }
                _ => println!("Otro tipo de evento: {:?}", event),
            }
        }
    });

    // Suscribirse a símbolos
    println!("Suscribiéndose a SPY y AAPL...");
    let subscriptions = vec![
        FeedSubscription {
            event_type: "Quote".to_string(),
            symbol: "SPY".to_string(),
            from_time: None,
            source: None,
        },
        FeedSubscription {
            event_type: "Trade".to_string(),
            symbol: "SPY".to_string(),
            from_time: None,
            source: None,
        },
        FeedSubscription {
            event_type: "Quote".to_string(),
            symbol: "AAPL".to_string(),
            from_time: None,
            source: None,
        },
    ];

    client.subscribe(channel_id, subscriptions).await?;
    println!("Suscripción exitosa");

    // Mantener la conexión activa por un tiempo
    println!("Recibiendo datos durante 2 minutos...");
    sleep(Duration::from_secs(120)).await;

    // Desconectar
    println!("Desconectando...");
    client.disconnect().await?;
    println!("Desconexión exitosa");

    Ok(())
}
