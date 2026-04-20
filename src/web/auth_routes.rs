use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Form, Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, StatusCode};
use tracing::{info, warn};

use super::AppState;
use super::security::{
    csrf_forbidden_response, extract_client_ip, is_same_origin_request, sanitize_next,
};
use super::templates::render_login;
use crate::auth::{LoginResult, append_set_cookie};
use crate::http::redirect_response;

#[derive(serde::Deserialize)]
pub(crate) struct LoginQuery {
    pub(crate) next: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct LoginForm {
    pub(crate) password: String,
    pub(crate) next: Option<String>,
}

pub(crate) async fn login_page(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> axum::response::Response {
    render_login(&state, query.next.as_deref(), None, StatusCode::OK)
}

pub(crate) async fn login_submit(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> axum::response::Response {
    if !is_same_origin_request(&state.config, &headers, true) {
        warn!("rejected cross-origin login submission");
        return csrf_forbidden_response();
    }

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

pub(crate) async fn logout_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> axum::response::Response {
    if !is_same_origin_request(&state.config, &headers, true) {
        warn!("rejected cross-origin logout submission");
        return csrf_forbidden_response();
    }

    state.auth.logout_from_headers(&headers).await;

    let mut response = redirect_response("/_auth/login", Some(&headers));
    response
        .headers_mut()
        .append(SET_COOKIE, state.auth.clear_cookie_header());
    response
}
