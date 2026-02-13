use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, Form, Query, Request, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use serde::Deserialize;
use tera::{Context, Tera};
use tracing::{error, info, warn};

use crate::auth::{AuthService, LoginResult, append_set_cookie};
use crate::config::Config;
use crate::proxy::ProxyService;
use crate::wake::{EnsureReadyResult, WakeCoordinator};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    templates: Arc<Tera>,
    auth: Arc<AuthService>,
    wake: Arc<WakeCoordinator>,
    proxy: Arc<ProxyService>,
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

pub fn load_templates() -> anyhow::Result<Tera> {
    let tera = Tera::new("templates/**/*.html")?;
    Ok(tera)
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
            let mut response = Redirect::to(next.as_deref().unwrap_or("/")).into_response();
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

    let mut response = Redirect::to("/_auth/login").into_response();
    response
        .headers_mut()
        .append(SET_COOKIE, state.auth.clear_cookie_header());
    response
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
    Redirect::to(&target).into_response()
}

pub async fn proxy_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Request,
) -> axum::response::Response {
    let client_ip = extract_client_ip(&state.config, &headers, addr);

    match state
        .wake
        .ensure_ready(state.config.edge_wait_timeout)
        .await
    {
        EnsureReadyResult::Ready { cycle_id } => {
            info!(cycle_id, auth = true, "upstream is ready, proxying request");
        }
        EnsureReadyResult::StillWaking {
            cycle_id,
            state: wake_state,
        } => {
            info!(cycle_id, ?wake_state, "still waking upstream");
            let mut context = Context::new();
            context.insert("cycle_id", &cycle_id);
            context.insert("state", &format!("{wake_state:?}"));
            return render_template(&state, "still-waking.html", context, StatusCode::ACCEPTED);
        }
        EnsureReadyResult::TimedOut { cycle_id } => {
            warn!(cycle_id, "wake timeout reached");
            let mut context = Context::new();
            context.insert("cycle_id", &cycle_id);
            context.insert("timeout_secs", &state.config.wake_timeout.as_secs());
            return render_template(
                &state,
                "wake-timeout.html",
                context,
                StatusCode::GATEWAY_TIMEOUT,
            );
        }
        EnsureReadyResult::Failed { cycle_id, reason } => {
            error!(cycle_id, %reason, "wake cycle failed");
            let mut context = Context::new();
            context.insert("cycle_id", &cycle_id);
            context.insert("error", &reason);
            return render_template(
                &state,
                "upstream-unhealthy.html",
                context,
                StatusCode::BAD_GATEWAY,
            );
        }
    }

    match state.proxy.forward(request, client_ip).await {
        Ok(response) => response,
        Err(error) => {
            error!(?error, "proxy request failed");
            let mut context = Context::new();
            context.insert("error", &error.to_string());
            render_template(&state, "error.html", context, StatusCode::BAD_GATEWAY)
        }
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
    render_template(state, "login.html", context, status)
}

fn render_template(
    state: &AppState,
    name: &str,
    context: Context,
    status: StatusCode,
) -> axum::response::Response {
    match state.templates.render(name, &context) {
        Ok(rendered) => {
            let mut response = rendered.into_response();
            *response.status_mut() = status;
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            response
        }
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

fn extract_client_ip(config: &Config, headers: &HeaderMap, connect_info: SocketAddr) -> IpAddr {
    if !config.trust_proxy_headers || !config.trusted_proxy_ips.contains(&connect_info.ip()) {
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

fn sanitize_next(next: Option<&str>) -> Option<String> {
    let Some(next) = next else {
        return None;
    };

    if next.starts_with('/') && !next.starts_with("//") {
        Some(next.to_string())
    } else {
        None
    }
}
