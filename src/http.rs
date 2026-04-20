use axum::body::Body;
use axum::http::header::{ACCEPT, CACHE_CONTROL, EXPIRES, PRAGMA, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};

pub(crate) fn apply_gateway_security_headers(headers: &mut HeaderMap) {
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("accelerometer=(), camera=(), geolocation=(), gyroscope=(), microphone=(), payment=(), usb=()"),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
}

pub(crate) fn mark_uncacheable(headers: &mut HeaderMap) {
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    headers.insert(PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(EXPIRES, HeaderValue::from_static("0"));
}

pub(crate) fn redirect_response(target: &str, request_headers: Option<&HeaderMap>) -> Response {
    if request_headers.is_some_and(should_use_soft_redirect) {
        return soft_redirect_response(target);
    }

    Redirect::to(target).into_response()
}

pub(crate) fn should_use_soft_redirect(headers: &HeaderMap) -> bool {
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

pub(crate) fn soft_redirect_response(target: &str) -> Response {
    let escaped_target = escape_js_string(target);
    let body = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1,viewport-fit=cover\"><meta name=\"mobile-web-app-capable\" content=\"yes\"><meta name=\"apple-mobile-web-app-capable\" content=\"yes\"><meta name=\"apple-mobile-web-app-title\" content=\"wakeproxy\"><meta name=\"apple-mobile-web-app-status-bar-style\" content=\"black-translucent\"><meta name=\"theme-color\" content=\"#eef4ed\"><link rel=\"manifest\" href=\"/manifest.webmanifest\"><link rel=\"apple-touch-icon\" sizes=\"180x180\" href=\"/apple-touch-icon.png\"><title>Redirecting</title></head><body><script>window.location.replace(\"{escaped_target}\");</script><noscript>Redirecting...</noscript></body></html>",
    );
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    apply_gateway_security_headers(response.headers_mut());
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
