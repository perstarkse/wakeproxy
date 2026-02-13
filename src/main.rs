use std::sync::Arc;

use axum::Router;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;
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
    let app = build_router(state.clone());

    let listener = TcpListener::bind(state.config.bind_addr).await?;
    info!(bind_addr = %state.config.bind_addr, "wol-web-proxy listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}

fn build_router(state: AppState) -> Router {
    let protected_router = Router::new()
        .fallback(axum::routing::any(web::proxy_handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            web::auth_guard_middleware,
        ));

    Router::new()
        .route("/_auth/login", axum::routing::get(web::login_page))
        .route("/_auth/login", axum::routing::post(web::login_submit))
        .route("/_auth/logout", axum::routing::post(web::logout_submit))
        .route("/_health", axum::routing::get(web::gateway_health))
        .merge(protected_router)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                .layer(PropagateRequestIdLayer::x_request_id())
                .layer(TraceLayer::new_for_http()),
        )
        .with_state(state)
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
