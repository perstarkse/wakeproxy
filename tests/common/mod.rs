#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHasher, SaltString};
use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::extract::{
    Request,
    ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade},
};
use axum::http::header::{HOST, SET_COOKIE};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::{Router, serve};
use futures_util::StreamExt;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use reqwest::redirect::Policy;
use tera::Tera;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use wol_web_proxy::config::Config;
use wol_web_proxy::readiness::ReadinessProbe;
use wol_web_proxy::wake::{WakeConfig, WakeCoordinator, WakePacketSender, WakeSendError};
use wol_web_proxy::web::{self, AppState};

pub struct ServerHandle {
    pub addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl ServerHandle {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.task.abort();
    }
}

#[derive(Clone)]
pub struct AlwaysReadyProbe;

#[async_trait]
impl ReadinessProbe for AlwaysReadyProbe {
    async fn ping(&self) -> bool {
        true
    }

    async fn health(&self) -> bool {
        true
    }
}

#[derive(Clone)]
pub struct NeverReadyProbe;

#[async_trait]
impl ReadinessProbe for NeverReadyProbe {
    async fn ping(&self) -> bool {
        false
    }

    async fn health(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct OkWakeSender;

#[async_trait]
impl WakePacketSender for OkWakeSender {
    async fn send_magic_packet(&self) -> Result<(), WakeSendError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct FailingWakeSender;

#[async_trait]
impl WakePacketSender for FailingWakeSender {
    async fn send_magic_packet(&self) -> Result<(), WakeSendError> {
        Err(WakeSendError::Message(
            "simulated wake sender failure".to_string(),
        ))
    }
}

pub fn build_config(upstream_port: u16, password: &str) -> Config {
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("hash")
        .to_string();

    let mut map = HashMap::new();
    map.insert(
        "WOL_PROXY_UPSTREAM_HOST".to_string(),
        "127.0.0.1".to_string(),
    );
    map.insert(
        "WOL_PROXY_UPSTREAM_PORT".to_string(),
        upstream_port.to_string(),
    );
    map.insert("WOL_PROXY_HEALTH_PATH".to_string(), "/health".to_string());
    map.insert(
        "WOL_PROXY_WOL_MAC".to_string(),
        "aa:bb:cc:dd:ee:ff".to_string(),
    );
    map.insert(
        "WOL_PROXY_WOL_BROADCAST_IP".to_string(),
        "127.255.255.255".to_string(),
    );
    map.insert("WOL_PROXY_PASSWORD_HASH".to_string(), password_hash);
    map.insert(
        "WOL_PROXY_SESSION_SECRET".to_string(),
        "integration-test-secret".to_string(),
    );
    map.insert("WOL_PROXY_BIND_ADDR".to_string(), "127.0.0.1:0".to_string());
    map.insert("WOL_PROXY_COOKIE_SECURE".to_string(), "false".to_string());
    map.insert(
        "WOL_PROXY_LOGIN_MAX_FAILURES".to_string(),
        "1000".to_string(),
    );

    Config::from_map(&map).expect("valid config")
}

pub fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

pub async fn login_and_extract_cookie(
    client: &reqwest::Client,
    base_url: &str,
    password: &str,
) -> String {
    let response = client
        .post(format!("{base_url}/_auth/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("password={password}&next=%2F"))
        .send()
        .await
        .expect("login response");

    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);

    response
        .headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::to_string)
        .expect("session cookie")
}

pub async fn spawn_gateway<S, P>(config: Config, sender: S, probe: P) -> ServerHandle
where
    S: WakePacketSender + 'static,
    P: ReadinessProbe + 'static,
{
    let templates = Arc::new(web::load_templates().expect("templates"));
    spawn_gateway_with_templates(config, sender, probe, templates).await
}

pub async fn spawn_gateway_with_templates<S, P>(
    config: Config,
    sender: S,
    probe: P,
    templates: Arc<Tera>,
) -> ServerHandle
where
    S: WakePacketSender + 'static,
    P: ReadinessProbe + 'static,
{
    let config = Arc::new(config);
    let wake = Arc::new(WakeCoordinator::new(
        WakeConfig {
            wake_timeout: config.wake_timeout,
            poll_interval: config.poll_interval,
            ready_cache_ttl: config.ready_cache_ttl,
        },
        Arc::new(sender),
        Arc::new(probe),
    ));

    let proxy_client: Client<HttpConnector, Body> =
        Client::builder(TokioExecutor::new()).build(HttpConnector::new());

    let state = AppState::new(config, templates, wake, proxy_client);
    let app = web::build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind gateway");
    let addr = listener.local_addr().expect("gateway local addr");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await;
    });

    ServerHandle {
        addr,
        shutdown_tx: Some(shutdown_tx),
        task,
    }
}

pub async fn spawn_upstream() -> ServerHandle {
    async fn health() -> &'static str {
        "ok"
    }

    async fn echo(request: Request<Body>) -> String {
        let (parts, body) = request.into_parts();
        let body_bytes = to_bytes(body, 1024 * 1024).await.unwrap_or_default();
        let body_text = String::from_utf8_lossy(&body_bytes);

        let xff = parts
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let xfp = parts
            .headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let xfh = parts
            .headers
            .get("x-forwarded-host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        format!(
            "method={};path={};query={};xff={};xfp={};xfh={};body={}",
            parts.method,
            parts.uri.path(),
            parts.uri.query().unwrap_or(""),
            xff,
            xfp,
            xfh,
            body_text
        )
    }

    async fn ws_echo(ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(handle_ws_echo)
    }

    async fn redirect_relative() -> impl IntoResponse {
        axum::response::Redirect::to("/demo?from=relative")
    }

    async fn redirect_absolute(headers: Request<Body>) -> impl IntoResponse {
        let host = headers
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("127.0.0.1");
        let target = format!("http://{host}/demo?from=absolute");
        axum::response::Redirect::to(&target)
    }

    async fn force_101() -> impl IntoResponse {
        http::StatusCode::SWITCHING_PROTOCOLS
    }

    async fn handle_ws_echo(mut socket: WebSocket) {
        while let Some(message) = socket.next().await {
            let Ok(message) = message else {
                break;
            };

            match message {
                AxumWsMessage::Text(text) => {
                    if text.as_str() == "close-me" {
                        let _ = socket.send(AxumWsMessage::Close(None)).await;
                        break;
                    }

                    let response = format!("upstream:{text}");
                    if socket
                        .send(AxumWsMessage::Text(response.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                AxumWsMessage::Binary(bytes) => {
                    if socket.send(AxumWsMessage::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                AxumWsMessage::Close(_) => break,
                _ => {}
            }
        }
    }

    let app = Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws_echo))
        .route("/force-101", get(force_101))
        .route("/redirect-rel", get(redirect_relative))
        .route("/redirect-abs", get(redirect_absolute))
        .fallback(any(echo));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("upstream local addr");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let task = tokio::spawn(async move {
        let _ = serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    ServerHandle {
        addr,
        shutdown_tx: Some(shutdown_tx),
        task,
    }
}
