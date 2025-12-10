use dxlink::{DXLinkClient, EventType, FeedSubscription};
use std::env;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tracing::info;

#[tokio::test]
async fn test_integration_with_real_server() {
    let token = env::var("DXLINK_API_TOKEN").unwrap_or_else(|_| String::new());
    let url = "wss://demo.dxfeed.com/dxlink-ws";

    info!("Conectando al servidor demo de DXFeed: {}", url);

    let mut client = DXLinkClient::new(url, &token);

    let mut event_stream = client
        .connect()
        .await
        .expect("Fallo al conectar al servidor");

    let channel_id = client
        .create_feed_channel("AUTO")
        .await
        .expect("Fallo al crear canal de feed");
    info!("Canal de feed creado: {}", channel_id);

    client
        .setup_feed(
            channel_id,
            &[
                EventType::Quote,
                EventType::Trade,
                EventType::Greeks,
                EventType::TimeAndSale,
            ],
        )
        .await
        .expect("Fallo al configurar feed");

    let event_counter = Arc::new(Mutex::new(0));
    let event_counter_clone = event_counter.clone();

    let stream_task = tokio::spawn(async move {
        info!("Iniciando procesamiento de stream de eventos");
        let mut local_counter = 0;

        while local_counter < 5 {
            match timeout(Duration::from_secs(3), event_stream.recv()).await {
                Ok(Some(event)) => {
                    local_counter += 1;
                    info!("Evento recibido: {:?}", event);
                    let mut counter = event_counter_clone.lock().unwrap();
                    *counter += 1;
                }
                Ok(None) => {
                    info!("Stream de eventos cerrado");
                    break;
                }
                Err(_) => {
                    info!("Timeout esperando evento");
                    break;
                }
            }
        }

        info!(
            "Procesamiento de eventos completado. Eventos recibidos: {}",
            local_counter
        );
        local_counter
    });

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

    client
        .subscribe(channel_id, subscriptions)
        .await
        .expect("Fallo al suscribirse");

    let wait_duration = Duration::from_secs(15);
    info!("Esperando {} segundos", wait_duration.as_secs());
    sleep(wait_duration).await;

    stream_task.abort();
    client.disconnect().await.expect("Fallo al desconectar");

    let event_count = *event_counter.lock().unwrap();
    info!("Total de eventos recibidos: {}", event_count);
    info!("Test completado exitosamente");
}
