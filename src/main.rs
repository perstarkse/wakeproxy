use std::sync::Arc;

use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;
use wol_web_proxy::config::Config;
use wol_web_proxy::readiness::SystemReadinessProbe;
use wol_web_proxy::wake::{UdpWakeSender, WakeConfig, WakeCoordinator};
use wol_web_proxy::web::{self, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = Arc::new(Config::from_env()?);
    let templates = Arc::new(web::load_templates()?);

    let http_client: Client<HttpConnector, axum::body::Body> =
        Client::builder(TokioExecutor::new()).build(HttpConnector::new());

    let readiness_probe = Arc::new(SystemReadinessProbe::new(&config)?);
    let wake_sender = Arc::new(UdpWakeSender::new(
        config.wol_mac,
        config.wol_broadcast_ip,
        config.wol_broadcast_port,
    ));
    let wake_coordinator = Arc::new(WakeCoordinator::new(
        WakeConfig {
            wake_timeout: config.wake_timeout,
            poll_interval: config.poll_interval,
            ready_cache_ttl: config.ready_cache_ttl,
        },
        wake_sender,
        readiness_probe,
    ));

    let state = AppState::new(config, templates, wake_coordinator, http_client);
    let app = web::build_router(state.clone());

    let listener = TcpListener::bind(state.config.bind_addr).await?;
    info!(bind_addr = %state.config.bind_addr, "wol-web-proxy listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_thread_ids(true)
        .json()
        .init();
}
