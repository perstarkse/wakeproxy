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
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::{Router, serve};
use futures_util::{SinkExt, StreamExt};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use reqwest::redirect::Policy;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async, tungstenite};
use wol_web_proxy::config::Config;
use wol_web_proxy::readiness::ReadinessProbe;
use wol_web_proxy::wake::{WakeConfig, WakeCoordinator, WakePacketSender};
use wol_web_proxy::web::{self, AppState};

struct ServerHandle {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl ServerHandle {
    fn url(&self) -> String {
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
struct AlwaysReadyProbe;

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
struct NeverReadyProbe;

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
struct OkWakeSender;

#[async_trait]
impl WakePacketSender for OkWakeSender {
    async fn send_magic_packet(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct FailingWakeSender;

#[async_trait]
impl WakePacketSender for FailingWakeSender {
    async fn send_magic_packet(&self) -> anyhow::Result<()> {
        anyhow::bail!("simulated wake sender failure")
    }
}

#[tokio::test]
async fn unauthenticated_proxy_request_redirects_to_login() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-1");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let response = client
        .get(format!("{}/demo?x=1", gateway.url()))
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/_auth/login?next=%2Fdemo%3Fx%3D1")
    );
}

#[tokio::test]
async fn login_and_proxy_forward_request_with_forwarded_headers() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-2");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-2").await;

    let response = client
        .post(format!("{}/demo?from=e2e", gateway.url()))
        .header(COOKIE, &cookie)
        .body("hello-e2e")
        .send()
        .await
        .expect("proxied request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.expect("body");

    assert!(body.contains("method=POST"));
    assert!(body.contains("path=/demo"));
    assert!(body.contains("query=from=e2e"));
    assert!(body.contains("body=hello-e2e"));
    assert!(body.contains("xfp=https"));
    assert!(body.contains("xfh=127.0.0.1:"));
}

#[tokio::test]
async fn logout_clears_session_and_requires_new_login() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-3");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-3").await;

    let logout = client
        .post(format!("{}/_auth/logout", gateway.url()))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("logout response");

    assert_eq!(logout.status(), reqwest::StatusCode::SEE_OTHER);
    assert!(logout.headers().contains_key(SET_COOKIE));

    let after_logout = client
        .get(format!("{}/demo", gateway.url()))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("post logout response");

    assert_eq!(after_logout.status(), reqwest::StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn wake_sender_failure_returns_unhealthy_template() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-4");
    let gateway = spawn_gateway(config, FailingWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-4").await;

    let response = client
        .get(format!("{}/demo", gateway.url()))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    let body = response.text().await.expect("body");
    assert!(body.contains("Upstream is unhealthy"));
}

#[tokio::test]
async fn wake_timeout_returns_timeout_template() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-5");
    config.wake_timeout = Duration::from_millis(200);
    config.poll_interval = Duration::from_millis(40);
    config.edge_wait_timeout = Duration::from_secs(1);

    let gateway = spawn_gateway(config, OkWakeSender, NeverReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-5").await;

    let response = client
        .get(format!("{}/demo", gateway.url()))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::GATEWAY_TIMEOUT);
    let body = response.text().await.expect("body");
    assert!(body.contains("did not wake in time"));
}

#[tokio::test]
async fn spoofed_xff_does_not_bypass_login_lockout_by_default() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-6");
    config.login_max_failures = 2;
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    for spoofed_ip in ["8.8.8.8", "1.1.1.1"] {
        let response = client
            .post(format!("{}/_auth/login", gateway.url()))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", spoofed_ip)
            .body("password=wrong")
            .send()
            .await
            .expect("login response");

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    let locked = client
        .post(format!("{}/_auth/login", gateway.url()))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("x-forwarded-for", "9.9.9.9")
        .body("password=password-6")
        .send()
        .await
        .expect("locked response");

    assert_eq!(locked.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn unauthenticated_websocket_upgrade_is_rejected_by_auth_guard() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-7");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;

    let ws_url = format!("ws://{}/ws", gateway.addr);
    let error = connect_async(&ws_url)
        .await
        .expect_err("unauthenticated websocket must fail");

    match error {
        tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), http::StatusCode::SEE_OTHER);
        }
        other => panic!("unexpected websocket error: {other:?}"),
    }
}

#[tokio::test]
async fn authenticated_websocket_request_is_proxied_to_upstream() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-8");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-8").await;
    let ws_url = format!("ws://{}/ws", gateway.addr);
    let mut request = ws_url.into_client_request().expect("ws request");
    request
        .headers_mut()
        .insert(COOKIE, cookie.parse().expect("cookie header value"));

    let (mut socket, response) = connect_async(request).await.expect("websocket connect");
    assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);

    socket
        .send(tungstenite::Message::Text("proxy-ws-check".into()))
        .await
        .expect("send websocket frame");

    let message = socket
        .next()
        .await
        .expect("receive frame")
        .expect("frame is ok");

    match message {
        tungstenite::Message::Text(text) => {
            assert_eq!(text, "upstream:proxy-ws-check");
        }
        other => panic!("unexpected websocket message: {other:?}"),
    }

    socket.close(None).await.expect("close websocket");
}

#[tokio::test]
async fn authenticated_websocket_binary_frame_is_proxied_to_upstream() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-9");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-9").await;
    let ws_url = format!("ws://{}/ws", gateway.addr);
    let mut request = ws_url.into_client_request().expect("ws request");
    request
        .headers_mut()
        .insert(COOKIE, cookie.parse().expect("cookie header value"));

    let (mut socket, response) = connect_async(request).await.expect("websocket connect");
    assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);

    let payload = vec![1_u8, 2, 3, 4, 5, 255];
    socket
        .send(tungstenite::Message::Binary(payload.clone().into()))
        .await
        .expect("send websocket binary frame");

    let message = socket
        .next()
        .await
        .expect("receive frame")
        .expect("frame is ok");

    match message {
        tungstenite::Message::Binary(bytes) => {
            assert_eq!(bytes.as_ref(), payload.as_slice());
        }
        other => panic!("unexpected websocket message: {other:?}"),
    }

    socket.close(None).await.expect("close websocket");
}

#[tokio::test]
async fn authenticated_websocket_close_from_upstream_reaches_client() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-10");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-10").await;
    let ws_url = format!("ws://{}/ws", gateway.addr);
    let mut request = ws_url.into_client_request().expect("ws request");
    request
        .headers_mut()
        .insert(COOKIE, cookie.parse().expect("cookie header value"));

    let (mut socket, response) = connect_async(request).await.expect("websocket connect");
    assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);

    socket
        .send(tungstenite::Message::Text("close-me".into()))
        .await
        .expect("send close trigger message");

    let message = socket
        .next()
        .await
        .expect("receive frame")
        .expect("frame is ok");

    match message {
        tungstenite::Message::Close(_) => {}
        other => panic!("unexpected websocket message: {other:?}"),
    }
}

fn build_config(upstream_port: u16, password: &str) -> Config {
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

fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

async fn login_and_extract_cookie(
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

async fn spawn_gateway<S, P>(config: Config, sender: S, probe: P) -> ServerHandle
where
    S: WakePacketSender + 'static,
    P: ReadinessProbe + 'static,
{
    let config = Arc::new(config);
    let templates = Arc::new(web::load_templates().expect("templates"));
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

    let protected =
        Router::new()
            .fallback(any(web::proxy_handler))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                web::auth_guard_middleware,
            ));

    let app = Router::new()
        .route("/_auth/login", get(web::login_page))
        .route("/_auth/login", post(web::login_submit))
        .route("/_auth/logout", post(web::logout_submit))
        .route("/_health", get(web::gateway_health))
        .merge(protected)
        .with_state(state);

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

async fn spawn_upstream() -> ServerHandle {
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
