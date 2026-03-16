use std::{env, net::SocketAddr};

use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use yv_streamer_software::{
    app::build_router,
    manager::CameraManager,
    startup::{self, LOG_LEVEL_ENV},
};

#[tokio::main]
async fn main() {
    let log_filter = startup::resolve_log_filter(
        env::var(LOG_LEVEL_ENV).ok(),
        env::var("RUST_LOG").ok(),
    );

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_new(log_filter.clone())
                .unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!(
        "{}={}",
        LOG_LEVEL_ENV,
        env::var(LOG_LEVEL_ENV).unwrap_or_else(|_| "<unset>".to_string())
    );

    let host = env::var("YV_STREAMER_SOFTWARE_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = env::var("YV_STREAMER_SOFTWARE_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8080);

    let address: SocketAddr = format!("{host}:{port}")
        .parse()
        .expect("failed to parse listen address");
    let listener = TcpListener::bind(address)
        .await
        .expect("failed to bind listener");

    let manager = CameraManager::default();

    if startup::should_emit_debug_boot_report(&log_filter) {
        startup::log_debug_boot_report(&manager, &host, port);
    }

    tracing::info!("yv-streamer-software listening on {}", address);

    axum::serve(listener, build_router(manager))
        .await
        .expect("server exited unexpectedly");
}
