use gateway::{GatewayServer, UdpOrderSender, UdpTransportConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber;

fn main() {
    use std::io::Write;

    eprintln!("GATEWAY MAIN STARTED");

    std::panic::set_hook(Box::new(|panic_info| {
        let _ = std::io::stderr().write_all(format!("PANIC: {:?}\n", panic_info).as_bytes());
        let _ = std::io::stderr().flush();
        std::process::exit(1);
    }));

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to create runtime: {}", e);
            std::process::exit(1);
        }
    };

    match rt.block_on(tokio_main()) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

async fn tokio_main() -> anyhow::Result<()> {
    // Load .env file if present (for local development)
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "gateway=info".into()),
        )
        .init();

    tracing::info!("Starting gateway service with UDP transport...");

    let accounts_url = std::env::var("ACCOUNTS_URL").unwrap_or_else(|_| "http://localhost:3001".to_string());
    let market_data_url = std::env::var("MARKET_DATA_URL").unwrap_or_else(|_| "http://localhost:3002".to_string());
    let bind_addr: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_string())
        .parse()?;

    // UDP transport configuration
    let udp_config = UdpTransportConfig::from_env();
    tracing::info!("UDP config: orders to {}, events from {}",
        udp_config.matching_engine_addr,
        udp_config.event_receiver_bind);

    tracing::info!("Proxying accounts requests to: {}", accounts_url);
    tracing::info!("Proxying market data requests to: {}", market_data_url);

    // Create UDP order sender
    let order_sender = match UdpOrderSender::new(
        udp_config.matching_engine_addr,
        udp_config.order_sender_bind,
    ) {
        Ok(s) => {
            tracing::info!("UDP order sender created");
            Arc::new(s)
        }
        Err(e) => {
            tracing::error!("Failed to create UDP order sender: {}", e);
            return Err(e);
        }
    };

    let server = GatewayServer::new(order_sender, accounts_url, market_data_url);

    server.start_event_broadcaster().await;

    // Start UDP event receiver
    if let Err(e) = server.start_udp_event_receiver(udp_config.event_receiver_bind).await {
        tracing::warn!("Failed to start UDP event receiver (continuing without it): {}", e);
    }

    let app = server.router();

    tracing::info!("Gateway server listening on {}", bind_addr);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!("Server bound, starting to serve...");

    axum::serve(listener, app).await?;

    Ok(())
}

