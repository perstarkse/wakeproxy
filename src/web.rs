use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Form, Query, Request, State};
use axum::http::header::{
    ACCEPT, CACHE_CONTROL, EXPIRES, PRAGMA, RETRY_AFTER, SET_COOKIE, USER_AGENT,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect};
use axum::routing::{any, get, post};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};
use thiserror::Error;
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

use crate::auth::{AuthService, LoginResult, append_set_cookie};
use crate::config::Config;
use crate::proxy::{ProxyError, ProxyService};
use crate::wake::{WakeCoordinator, WakeStartResult, WakeState};

const PWA_ICON_192_PNG: &[u8] = include_bytes!("../assets/icons/icon-192.png");
const PWA_ICON_512_PNG: &[u8] = include_bytes!("../assets/icons/icon-512.png");
const PWA_ICON_MASKABLE_512_PNG: &[u8] = include_bytes!("../assets/icons/icon-maskable-512.png");
const APPLE_TOUCH_ICON_PNG: &[u8] = include_bytes!("../assets/icons/apple-touch-icon.png");
const PWA_SERVICE_WORKER_JS: &str = r#"const STATIC_CACHE_NAME = "wake-static-v1";
const STATIC_CACHE_ASSETS = [
  "/manifest.webmanifest",
  "/apple-touch-icon.png",
  "/icons/icon-192.png",
  "/icons/icon-512.png",
  "/icons/icon-maskable-512.png"
];

const OFFLINE_HTML = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Offline</title><style>body{font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",sans-serif;background:#eef4ed;color:#1b2a20;margin:0;display:grid;place-items:center;min-height:100vh;padding:1rem}main{max-width:32rem;background:#fffdf8;border:1px solid #d4e1d1;border-radius:14px;padding:1rem 1.2rem}h1{margin:.1rem 0 .5rem;font-size:1.25rem}p{margin:0;line-height:1.5}</style></head><body><main><h1>Offline</h1><p>This shortcut needs a network connection to reach your secured gateway. Reconnect and try again.</p></main></body></html>";

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(STATIC_CACHE_NAME)
      .then((cache) => cache.addAll(STATIC_CACHE_ASSETS))
      .then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== STATIC_CACHE_NAME).map((key) => caches.delete(key))))
      .then(() => self.clients.claim())
  );
});

const cacheFirst = async (request) => {
  const cache = await caches.open(STATIC_CACHE_NAME);
  const cached = await cache.match(request);
  if (cached) {
    return cached;
  }

  const response = await fetch(request);
  if (response && response.ok) {
    cache.put(request, response.clone());
  }
  return response;
};

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") {
    return;
  }

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) {
    return;
  }

  if (STATIC_CACHE_ASSETS.includes(url.pathname)) {
    event.respondWith(cacheFirst(request));
    return;
  }

  if (request.mode === "navigate") {
    event.respondWith(
      fetch(request).catch(
        () =>
          new Response(OFFLINE_HTML, {
            status: 503,
            headers: {
              "Content-Type": "text/html; charset=utf-8",
              "Cache-Control": "no-store"
            }
          })
      )
    );
    return;
  }

  event.respondWith(fetch(request));
});
"#;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    templates: Arc<Tera>,
    auth: Arc<AuthService>,
    wake: Arc<WakeCoordinator>,
    proxy: Arc<ProxyService>,
}

#[derive(Debug, Error)]
pub enum TemplateLoadError {
    #[error("failed to load template {name}")]
    Load {
        name: &'static str,
        #[source]
        source: tera::Error,
    },
}

#[derive(Debug, Error)]
pub enum TemplateRenderError {
    #[error("failed to render template {name}")]
    Render {
        name: &'static str,
        #[source]
        source: tera::Error,
    },
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Proxy(#[from] ProxyError),
    #[error(transparent)]
    Template(#[from] TemplateRenderError),
}

impl AppState {
    pub fn new(
        config: Arc<Config>,
        templates: Arc<Tera>,
        wake: Arc<WakeCoordinator>,
        proxy_client: Client<HttpConnector, Body>,
    ) -> Self {
        let auth = Arc::new(AuthService::new(&config));
        let proxy = Arc::new(ProxyService::new(config.clone(), proxy_client));

        Self {
            config,
            templates,
            auth,
            wake,
            proxy,
        }
    }
}

pub fn load_templates() -> Result<Tera, TemplateLoadError> {
    let mut tera = Tera::default();
    add_template(
        &mut tera,
        "_layout.html",
        include_str!("../templates/_layout.html"),
    )?;
    add_template(
        &mut tera,
        "login.html",
        include_str!("../templates/login.html"),
    )?;
    add_template(
        &mut tera,
        "wake-status.html",
        include_str!("../templates/wake-status.html"),
    )?;
    add_template(
        &mut tera,
        "error.html",
        include_str!("../templates/error.html"),
    )?;
    Ok(tera)
}

pub fn build_router(state: AppState) -> Router {
    let wake_router = Router::new()
        .route("/_wake", get(wake_page))
        .route("/_wake/start", post(wake_start_submit))
        .route("/_wake/poll", get(wake_poll))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_guard_middleware,
        ));

    let protected_router = Router::new()
        .fallback(any(proxy_handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            readiness_gate_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_guard_middleware,
        ));

    Router::new()
        .route("/_auth/login", get(login_page))
        .route("/_auth/login", post(login_submit))
        .route("/_auth/logout", post(logout_submit))
        .route("/_dev/wake-preview", get(wake_preview_page))
        .route("/manifest.webmanifest", get(web_manifest))
        .route("/sw.js", get(service_worker))
        .route("/apple-touch-icon.png", get(apple_touch_icon))
        .route("/icons/icon-192.png", get(pwa_icon_192))
        .route("/icons/icon-512.png", get(pwa_icon_512))
        .route("/icons/icon-maskable-512.png", get(pwa_icon_maskable_512))
        .route("/_health", get(gateway_health))
        .merge(wake_router)
        .merge(protected_router)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                .layer(PropagateRequestIdLayer::x_request_id())
                .layer(TraceLayer::new_for_http()),
        )
        .with_state(state)
}

#[derive(Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginForm {
    password: String,
    next: Option<String>,
}

#[derive(Deserialize)]
pub struct WakeQuery {
    next: Option<String>,
}

#[derive(Deserialize)]
pub struct WakeStartForm {
    next: Option<String>,
}

#[derive(Deserialize)]
pub struct WakePreviewQuery {
    next: Option<String>,
    state: Option<String>,
    reason: Option<String>,
    cycle_id: Option<u64>,
    auto: Option<String>,
}

#[derive(Serialize)]
pub struct WakePollPayload {
    state: &'static str,
    cycle_id: Option<u64>,
    ready: bool,
    redirect_to: Option<String>,
    message: &'static str,
    retryable: bool,
    reason: Option<String>,
}

#[derive(Serialize)]
struct WebManifest {
    name: &'static str,
    short_name: &'static str,
    id: &'static str,
    start_url: &'static str,
    scope: &'static str,
    display: &'static str,
    background_color: &'static str,
    theme_color: &'static str,
    description: &'static str,
    icons: Vec<WebManifestIcon>,
}

#[derive(Serialize)]
struct WebManifestIcon {
    src: &'static str,
    sizes: &'static str,
    #[serde(rename = "type")]
    mime_type: &'static str,
    purpose: &'static str,
}

pub async fn login_page(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> axum::response::Response {
    render_login(&state, query.next.as_deref(), None, StatusCode::OK)
}

pub async fn login_submit(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> axum::response::Response {
    let client_ip = extract_client_ip(&state.config, &headers, addr);
    let next = sanitize_next(form.next.as_deref());

    match state.auth.login(client_ip, &form.password).await {
        LoginResult::Success { cookie_header } => {
            info!(client_ip = %client_ip, "successful login");
            let mut response = redirect_response(next.as_deref().unwrap_or("/"), Some(&headers));
            append_set_cookie(response.headers_mut(), cookie_header);
            response
        }
        LoginResult::InvalidCredentials => {
            warn!(client_ip = %client_ip, "invalid login attempt");
            render_login(
                &state,
                next.as_deref(),
                Some("Invalid password"),
                StatusCode::UNAUTHORIZED,
            )
        }
        LoginResult::Locked { retry_after } => {
            warn!(client_ip = %client_ip, retry_after_secs = retry_after.as_secs(), "login locked");
            render_login(
                &state,
                next.as_deref(),
                Some("Too many attempts. Try again shortly."),
                StatusCode::TOO_MANY_REQUESTS,
            )
        }
    }
}

pub async fn logout_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> axum::response::Response {
    state.auth.logout_from_headers(&headers).await;

    let mut response = redirect_response("/_auth/login", Some(&headers));
    response
        .headers_mut()
        .append(SET_COOKIE, state.auth.clear_cookie_header());
    response
}

pub async fn wake_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WakeQuery>,
) -> axum::response::Response {
    let next = sanitize_next(query.next.as_deref()).unwrap_or_else(|| "/".to_string());
    let snapshot = state.wake.status_snapshot().await;

    if snapshot.ready {
        return redirect_response(&next, Some(&headers));
    }

    let mut context = Context::new();
    context.insert("next", &next);
    context.insert("poll_interval_ms", &state.config.wake_poll_interval_ms);
    context.insert("state", wake_state_label(snapshot.state));
    context.insert("status_message", wake_status_message(snapshot.state));
    context.insert("retryable", &wake_retryable(snapshot.state));
    context.insert("cycle_id", &snapshot.cycle_id.unwrap_or_default());
    context.insert("cycle_known", &snapshot.cycle_id.is_some());
    context.insert("reason", &snapshot.reason.unwrap_or_default());
    context.insert("preview_mode", &false);
    context.insert("preview_auto", &false);

    let mut response = render_template(&state, "wake-status.html", context, StatusCode::OK);
    mark_uncacheable(response.headers_mut());
    response
}

pub async fn wake_preview_page(
    State(state): State<AppState>,
    Query(query): Query<WakePreviewQuery>,
) -> axum::response::Response {
    #[cfg(not(debug_assertions))]
    {
        let WakePreviewQuery {
            next,
            state: preview_state,
            reason,
            cycle_id,
            auto,
        } = query;
        let _ = (state, next, preview_state, reason, cycle_id, auto);
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }

    #[cfg(debug_assertions)]
    {
        let next = sanitize_next(query.next.as_deref()).unwrap_or_else(|| "/demo".to_string());
        let preview_state = query
            .state
            .as_deref()
            .and_then(wake_state_from_label)
            .unwrap_or(WakeState::Sleeping);
        let preview_auto = query
            .auto
            .as_deref()
            .map(preview_auto_from_label)
            .unwrap_or(false);
        let reason = query.reason.unwrap_or_else(|| {
            if preview_state == WakeState::Failed {
                "Preview: upstream did not accept connections yet.".to_string()
            } else {
                "".to_string()
            }
        });

        let mut context = Context::new();
        context.insert("next", &next);
        context.insert("poll_interval_ms", &1200_u64);
        context.insert("state", wake_state_label(preview_state));
        context.insert("status_message", wake_status_message(preview_state));
        context.insert("retryable", &wake_retryable(preview_state));
        context.insert("cycle_id", &query.cycle_id.unwrap_or(4242));
        context.insert("cycle_known", &true);
        context.insert("reason", &reason);
        context.insert("preview_mode", &true);
        context.insert("preview_auto", &preview_auto);

        let mut response = render_template(&state, "wake-status.html", context, StatusCode::OK);
        mark_uncacheable(response.headers_mut());
        response
    }
}

pub async fn wake_start_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<WakeStartForm>,
) -> axum::response::Response {
    let next = sanitize_next(form.next.as_deref()).unwrap_or_else(|| "/".to_string());

    if let WakeStartResult::AlreadyReady { .. } = state.wake.start_wake().await {
        return redirect_response(&next, Some(&headers));
    }

    let encoded_next = urlencoding::encode(&next);
    let target = format!("/_wake?next={encoded_next}");
    redirect_response(&target, Some(&headers))
}

pub async fn wake_poll(
    State(state): State<AppState>,
    Query(query): Query<WakeQuery>,
) -> axum::response::Response {
    let next = sanitize_next(query.next.as_deref()).unwrap_or_else(|| "/".to_string());
    let snapshot = state.wake.status_snapshot().await;

    let payload = WakePollPayload {
        state: wake_state_label(snapshot.state),
        cycle_id: snapshot.cycle_id,
        ready: snapshot.ready,
        redirect_to: snapshot.ready.then_some(next),
        message: wake_status_message(snapshot.state),
        retryable: wake_retryable(snapshot.state),
        reason: snapshot.reason,
    };

    let mut response = axum::Json(payload).into_response();
    mark_uncacheable(response.headers_mut());
    response
}

pub async fn web_manifest() -> axum::response::Response {
    let mut response = axum::Json(WebManifest {
        name: "Wake Gateway",
        short_name: "Wake",
        id: "/",
        start_url: "/",
        scope: "/",
        display: "standalone",
        background_color: "#eef4ed",
        theme_color: "#eef4ed",
        description: "Secure wake-and-proxy gateway for your upstream app.",
        icons: vec![
            WebManifestIcon {
                src: "/icons/icon-192.png",
                sizes: "192x192",
                mime_type: "image/png",
                purpose: "any",
            },
            WebManifestIcon {
                src: "/icons/icon-512.png",
                sizes: "512x512",
                mime_type: "image/png",
                purpose: "any",
            },
            WebManifestIcon {
                src: "/icons/icon-maskable-512.png",
                sizes: "512x512",
                mime_type: "image/png",
                purpose: "any maskable",
            },
        ],
    })
    .into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/manifest+json"),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

pub async fn service_worker() -> axum::response::Response {
    let mut response = PWA_SERVICE_WORKER_JS.into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("service-worker-allowed", HeaderValue::from_static("/"));
    response
}

pub async fn pwa_icon_192() -> axum::response::Response {
    png_asset_response(PWA_ICON_192_PNG)
}

pub async fn pwa_icon_512() -> axum::response::Response {
    png_asset_response(PWA_ICON_512_PNG)
}

pub async fn pwa_icon_maskable_512() -> axum::response::Response {
    png_asset_response(PWA_ICON_MASKABLE_512_PNG)
}

pub async fn apple_touch_icon() -> axum::response::Response {
    png_asset_response(APPLE_TOUCH_ICON_PNG)
}

pub async fn gateway_health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

pub async fn auth_guard_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    if state
        .auth
        .authenticate_headers(request.headers())
        .await
        .is_some()
    {
        return next.run(request).await;
    }

    let encoded_next = urlencoding::encode(
        request
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/"),
    );
    let target = format!("/_auth/login?next={encoded_next}");
    redirect_response(&target, Some(request.headers()))
}

pub async fn readiness_gate_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let snapshot = state.wake.status_snapshot().await;
    if snapshot.ready {
        return next.run(request).await;
    }

    let encoded_next = urlencoding::encode(
        request
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/"),
    );

    if request.method() == Method::GET || request.method() == Method::HEAD {
        let target = format!("/_wake?next={encoded_next}");
        return redirect_response(&target, Some(request.headers()));
    }

    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        "Upstream is not ready. Open /_wake to wake and connect.",
    )
        .into_response();
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("3"));
    mark_uncacheable(response.headers_mut());
    response
}

pub async fn proxy_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Request,
) -> axum::response::Response {
    let trust_proxy = should_trust_proxy(&state.config, addr);
    let client_ip = extract_client_ip(&state.config, &headers, addr);

    info!(
        auth = true,
        "upstream assumed ready by readiness gate, proxying request"
    );

    match state.proxy.forward(request, client_ip, trust_proxy).await {
        Ok(response) => response,
        Err(error) => app_error_response(&state, error.into()),
    }
}

fn render_login(
    state: &AppState,
    next: Option<&str>,
    error: Option<&str>,
    status: StatusCode,
) -> axum::response::Response {
    let mut context = Context::new();
    context.insert("next", &next.unwrap_or("/"));
    context.insert("error", &error.unwrap_or(""));
    let mut response = render_template(state, "login.html", context, status);
    mark_uncacheable(response.headers_mut());
    response
}

fn render_template(
    state: &AppState,
    name: &'static str,
    context: Context,
    status: StatusCode,
) -> axum::response::Response {
    match try_render_template(state, name, context, status) {
        Ok(response) => response,
        Err(error) => {
            error!(template = name, ?error, "template render failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Template rendering failed",
            )
                .into_response()
        }
    }
}

fn add_template(
    tera: &mut Tera,
    name: &'static str,
    content: &'static str,
) -> Result<(), TemplateLoadError> {
    tera.add_raw_template(name, content)
        .map_err(|source| TemplateLoadError::Load { name, source })
}

fn try_render_template(
    state: &AppState,
    name: &'static str,
    context: Context,
    status: StatusCode,
) -> Result<axum::response::Response, TemplateRenderError> {
    let rendered = state
        .templates
        .render(name, &context)
        .map_err(|source| TemplateRenderError::Render { name, source })?;
    let mut response = rendered.into_response();
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    Ok(response)
}

fn app_error_response(state: &AppState, error: AppError) -> axum::response::Response {
    match error {
        AppError::Proxy(proxy_error) => {
            error!(?proxy_error, "proxy request failed");
            let mut context = Context::new();
            context.insert("error", &proxy_error.to_string());
            match try_render_template(state, "error.html", context, StatusCode::BAD_GATEWAY) {
                Ok(response) => response,
                Err(template_error) => app_error_response(state, template_error.into()),
            }
        }
        AppError::Template(template_error) => {
            error!(?template_error, "template render failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Template rendering failed",
            )
                .into_response()
        }
    }
}

fn png_asset_response(bytes: &'static [u8]) -> axum::response::Response {
    let mut response = bytes.to_vec().into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("image/png"),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=604800, immutable"),
    );
    response
}

fn extract_client_ip(config: &Config, headers: &HeaderMap, connect_info: SocketAddr) -> IpAddr {
    if !should_trust_proxy(config, connect_info) {
        return connect_info.ip();
    }

    if let Some(forwarded_for) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        && let Some(first) = forwarded_for.split(',').next()
        && let Ok(parsed) = first.trim().parse::<IpAddr>()
    {
        return parsed;
    }

    connect_info.ip()
}

fn should_trust_proxy(config: &Config, connect_info: SocketAddr) -> bool {
    config.trust_proxy_headers && config.trusted_proxy_ips.contains(&connect_info.ip())
}

fn wake_state_label(state: WakeState) -> &'static str {
    match state {
        WakeState::Sleeping => "sleeping",
        WakeState::Waking => "waking",
        WakeState::PingReady => "ping_ready",
        WakeState::HttpReady => "http_ready",
        WakeState::Ready => "ready",
        WakeState::TimedOut => "timed_out",
        WakeState::Failed => "failed",
    }
}

fn wake_status_message(state: WakeState) -> &'static str {
    match state {
        WakeState::Sleeping => "Remote system is asleep.",
        WakeState::Waking => "Starting your remote system.",
        WakeState::PingReady => "Host is reachable. Waiting for services.",
        WakeState::HttpReady => "Service checks passed. Finalizing connection.",
        WakeState::Ready => "Ready. Redirecting to your destination.",
        WakeState::TimedOut => "Wake timed out. You can retry.",
        WakeState::Failed => "Wake failed. You can retry.",
    }
}

fn wake_retryable(state: WakeState) -> bool {
    matches!(
        state,
        WakeState::Sleeping | WakeState::TimedOut | WakeState::Failed
    )
}

#[cfg(any(test, debug_assertions))]
fn wake_state_from_label(state: &str) -> Option<WakeState> {
    match state {
        "sleeping" => Some(WakeState::Sleeping),
        "waking" => Some(WakeState::Waking),
        "ping_ready" => Some(WakeState::PingReady),
        "http_ready" => Some(WakeState::HttpReady),
        "ready" => Some(WakeState::Ready),
        "timed_out" => Some(WakeState::TimedOut),
        "failed" => Some(WakeState::Failed),
        _ => None,
    }
}

#[cfg(any(test, debug_assertions))]
fn preview_auto_from_label(value: &str) -> bool {
    matches!(value, "1" | "true" | "yes" | "on")
}

fn mark_uncacheable(headers: &mut HeaderMap) {
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    headers.insert(PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(EXPIRES, HeaderValue::from_static("0"));
}

fn sanitize_next(next: Option<&str>) -> Option<String> {
    let next = next?;

    if next.starts_with('/') && !next.starts_with("//") {
        Some(next.to_string())
    } else {
        None
    }
}

fn redirect_response(
    target: &str,
    request_headers: Option<&HeaderMap>,
) -> axum::response::Response {
    if request_headers.is_some_and(should_use_soft_redirect) {
        return soft_redirect_response(target);
    }

    Redirect::to(target).into_response()
}

fn should_use_soft_redirect(headers: &HeaderMap) -> bool {
    is_ios_user_agent(headers) && is_document_navigation(headers)
}

fn is_ios_user_agent(headers: &HeaderMap) -> bool {
    let Some(user_agent) = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    let lower = user_agent.to_ascii_lowercase();
    lower.contains("iphone")
        || lower.contains("ipad")
        || lower.contains("ipod")
        || (lower.contains("macintosh") && lower.contains("mobile"))
}

fn is_document_navigation(headers: &HeaderMap) -> bool {
    if let Some(destination) = headers
        .get("sec-fetch-dest")
        .and_then(|value| value.to_str().ok())
        && destination.eq_ignore_ascii_case("document")
    {
        return true;
    }

    headers
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .map(|accept| accept.contains("text/html") || accept.contains("application/xhtml+xml"))
        .unwrap_or(false)
}

fn soft_redirect_response(target: &str) -> axum::response::Response {
    let escaped_target = escape_js_string(target);
    let body = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1,viewport-fit=cover\"><meta name=\"mobile-web-app-capable\" content=\"yes\"><meta name=\"apple-mobile-web-app-capable\" content=\"yes\"><meta name=\"apple-mobile-web-app-title\" content=\"Wake Gateway\"><meta name=\"apple-mobile-web-app-status-bar-style\" content=\"black-translucent\"><meta name=\"theme-color\" content=\"#eef4ed\"><link rel=\"manifest\" href=\"/manifest.webmanifest\"><link rel=\"apple-touch-icon\" sizes=\"180x180\" href=\"/apple-touch-icon.png\"><title>Redirecting</title></head><body><script>window.location.replace(\"{escaped_target}\");</script><noscript>Redirecting...</noscript></body></html>",
    );
    let mut response = body.into_response();
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    mark_uncacheable(response.headers_mut());
    response
}

fn escape_js_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '<' => escaped.push_str("\\u003C"),
            '>' => escaped.push_str("\\u003E"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, LOCATION, USER_AGENT};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};

    use super::{
        load_templates, mark_uncacheable, preview_auto_from_label, pwa_icon_192, redirect_response,
        sanitize_next, service_worker, wake_retryable, wake_state_from_label, wake_state_label,
        wake_status_message, web_manifest,
    };
    use crate::wake::WakeState;

    #[test]
    fn wake_state_labels_and_messages_cover_all_variants() {
        let cases = [
            (
                WakeState::Sleeping,
                "sleeping",
                "Remote system is asleep.",
                true,
            ),
            (
                WakeState::Waking,
                "waking",
                "Starting your remote system.",
                false,
            ),
            (
                WakeState::PingReady,
                "ping_ready",
                "Host is reachable. Waiting for services.",
                false,
            ),
            (
                WakeState::HttpReady,
                "http_ready",
                "Service checks passed. Finalizing connection.",
                false,
            ),
            (
                WakeState::Ready,
                "ready",
                "Ready. Redirecting to your destination.",
                false,
            ),
            (
                WakeState::TimedOut,
                "timed_out",
                "Wake timed out. You can retry.",
                true,
            ),
            (
                WakeState::Failed,
                "failed",
                "Wake failed. You can retry.",
                true,
            ),
        ];

        for (state, label, message, retryable) in cases {
            assert_eq!(wake_state_label(state), label);
            assert_eq!(wake_status_message(state), message);
            assert_eq!(wake_retryable(state), retryable);
        }
    }

    #[test]
    fn mark_uncacheable_sets_cache_headers() {
        let mut headers = HeaderMap::new();
        mark_uncacheable(&mut headers);

        assert_eq!(
            headers
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store, no-cache, must-revalidate")
        );
        assert_eq!(
            headers.get("pragma").and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
        assert_eq!(
            headers.get("expires").and_then(|value| value.to_str().ok()),
            Some("0")
        );
    }

    #[test]
    fn load_templates_registers_wake_status_and_sanitize_next_rejects_external_urls() {
        let tera = load_templates().expect("templates");
        let names = tera.get_template_names().collect::<Vec<_>>();
        assert!(names.contains(&"wake-status.html"));
        assert!(names.contains(&"login.html"));
        assert!(names.contains(&"error.html"));

        assert_eq!(sanitize_next(Some("/ok")), Some("/ok".to_string()));
        assert_eq!(sanitize_next(Some("//evil.example")), None);
        assert_eq!(sanitize_next(Some("https://evil.example")), None);
    }

    #[test]
    fn wake_state_from_label_round_trips_known_labels() {
        let cases = [
            ("sleeping", WakeState::Sleeping),
            ("waking", WakeState::Waking),
            ("ping_ready", WakeState::PingReady),
            ("http_ready", WakeState::HttpReady),
            ("ready", WakeState::Ready),
            ("timed_out", WakeState::TimedOut),
            ("failed", WakeState::Failed),
        ];

        for (label, expected) in cases {
            assert_eq!(wake_state_from_label(label), Some(expected));
        }

        assert_eq!(wake_state_from_label("unknown"), None);
    }

    #[test]
    fn preview_auto_from_label_accepts_truthy_values() {
        assert!(preview_auto_from_label("1"));
        assert!(preview_auto_from_label("true"));
        assert!(preview_auto_from_label("yes"));
        assert!(preview_auto_from_label("on"));
        assert!(!preview_auto_from_label("0"));
        assert!(!preview_auto_from_label("false"));
        assert!(!preview_auto_from_label("no"));
    }

    #[test]
    fn redirect_response_uses_http_redirect_for_non_ios_requests() {
        let headers = HeaderMap::new();
        let response = redirect_response("/target", Some(&headers));

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/target")
        );
    }

    #[tokio::test]
    async fn redirect_response_uses_soft_redirect_for_ios_document_navigation() {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15",
            ),
        );
        headers.insert(ACCEPT, HeaderValue::from_static("text/html"));

        let response = redirect_response("/target?x=1", Some(&headers));
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(LOCATION).is_none());

        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body bytes");
        let body_text = String::from_utf8_lossy(&body);
        assert!(body_text.contains("window.location.replace(\"/target?x=1\")"));
        assert!(body_text.contains("apple-mobile-web-app-capable"));
    }

    #[tokio::test]
    async fn web_manifest_exposes_standalone_shell_and_icons() {
        let response = web_manifest().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/manifest+json")
        );
        assert_eq!(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=3600")
        );

        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("manifest body");
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("\"display\":\"standalone\""));
        assert!(text.contains("\"src\":\"/icons/icon-192.png\""));
        assert!(text.contains("\"src\":\"/icons/icon-maskable-512.png\""));
        assert!(text.contains("\"purpose\":\"any maskable\""));
    }

    #[tokio::test]
    async fn pwa_icon_route_returns_png_with_cache_headers() {
        let response = pwa_icon_192().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/png")
        );
        assert_eq!(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=604800, immutable")
        );

        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("icon body");
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn service_worker_route_is_javascript_with_safe_strategy() {
        let response = service_worker().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/javascript; charset=utf-8")
        );
        assert_eq!(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );

        let body = to_bytes(response.into_body(), 256 * 1024)
            .await
            .expect("service worker body");
        let script = String::from_utf8_lossy(&body);
        assert!(script.contains("STATIC_CACHE_ASSETS"));
        assert!(script.contains("request.mode === \"navigate\""));
        assert!(script.contains("event.respondWith(fetch(request));"));
        assert!(script.contains("/manifest.webmanifest"));
    }
}
