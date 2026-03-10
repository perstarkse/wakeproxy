use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use tracing::info;

use super::AppState;
use super::security::{extract_client_ip, should_trust_proxy};
use super::templates::app_error_response;
use crate::http::{mark_uncacheable, redirect_response};

pub(crate) async fn auth_guard_middleware(
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

pub(crate) async fn readiness_gate_middleware(
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

pub(crate) async fn proxy_handler(
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
