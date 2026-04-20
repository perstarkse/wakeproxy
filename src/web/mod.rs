use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::routing::{any, get, post};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use tera::Tera;
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

mod assets;
mod auth_routes;
mod middleware;
mod security;
mod templates;
mod wake_routes;

pub use templates::TemplateLoadError;
pub use templates::load_templates;

use crate::auth::AuthService;
use crate::config::Config;
use crate::proxy::ProxyService;
use crate::wake::WakeCoordinator;

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

pub fn build_router(state: AppState) -> Router {
    let wake_router = Router::new()
        .route("/_wake", get(wake_routes::wake_page))
        .route("/_wake/start", post(wake_routes::wake_start_submit))
        .route("/_wake/poll", get(wake_routes::wake_poll))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_guard_middleware,
        ));

    let protected_router = Router::new()
        .fallback(any(middleware::proxy_handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::readiness_gate_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_guard_middleware,
        ));

    Router::new()
        .route("/_auth/login", get(auth_routes::login_page))
        .route("/_auth/login", post(auth_routes::login_submit))
        .route("/_auth/logout", post(auth_routes::logout_submit))
        .route("/_dev/wake-preview", get(wake_routes::wake_preview_page))
        .route("/manifest.webmanifest", get(assets::web_manifest))
        .route("/sw.js", get(assets::service_worker))
        .route("/apple-touch-icon.png", get(assets::apple_touch_icon))
        .route("/icons/icon-192.png", get(assets::pwa_icon_192))
        .route("/icons/icon-512.png", get(assets::pwa_icon_512))
        .route(
            "/icons/icon-maskable-512.png",
            get(assets::pwa_icon_maskable_512),
        )
        .route("/_health", get(assets::gateway_health))
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::SocketAddr;

    use axum::body::to_bytes;
    use axum::http::header::{
        ACCEPT, CACHE_CONTROL, CONTENT_TYPE, HOST, LOCATION, ORIGIN, REFERER, USER_AGENT,
    };
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::IntoResponse;

    use super::assets::{gateway_health, pwa_icon_192, service_worker, web_manifest};
    use super::load_templates;
    use super::security::{
        expected_origin_authority, extract_client_ip, is_same_origin_request, sanitize_next,
        websocket_origin_allowed,
    };
    use super::wake_routes::{
        preview_auto_from_label, wake_retryable, wake_state_from_label, wake_state_label,
        wake_status_message,
    };
    use crate::config::Config;
    use crate::http::{mark_uncacheable, redirect_response};
    use crate::wake::WakeState;

    fn trusted_proxy_config() -> Config {
        let mut map = HashMap::new();
        map.insert(
            "WAKEPROXY_UPSTREAM_HOST".to_string(),
            "127.0.0.1".to_string(),
        );
        map.insert("WAKEPROXY_UPSTREAM_PORT".to_string(), "3000".to_string());
        map.insert(
            "WAKEPROXY_WOL_MAC".to_string(),
            "aa:bb:cc:dd:ee:ff".to_string(),
        );
        map.insert(
            "WAKEPROXY_WOL_BROADCAST_IP".to_string(),
            "127.255.255.255".to_string(),
        );
        map.insert(
            "WAKEPROXY_PASSWORD_HASH".to_string(),
            "$argon2id$v=19$m=19456,t=2,p=1$dGVzdHRlc3R0ZXN0dGVzdA$0xDPfypM3Y76kJumWn95v9PoW7A1WeNseyX2VINeodQ"
                .to_string(),
        );
        map.insert(
            "WAKEPROXY_SESSION_SECRET".to_string(),
            "integration-test-secret".to_string(),
        );
        map.insert(
            "WAKEPROXY_TRUST_PROXY_HEADERS".to_string(),
            "true".to_string(),
        );
        map.insert(
            "WAKEPROXY_TRUSTED_PROXY_IPS".to_string(),
            "127.0.0.1,10.0.0.1".to_string(),
        );

        Config::from_map(&map).expect("config")
    }

    fn trusted_proxy_config_with_external_origin() -> Config {
        let mut config = trusted_proxy_config();
        config.external_origin = Some("https://wake.example.test".to_string());
        config
    }

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

    #[test]
    fn same_origin_request_accepts_matching_origin_and_rejects_cross_site() {
        let config = trusted_proxy_config();
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("gateway.local"));
        headers.insert(ORIGIN, HeaderValue::from_static("https://gateway.local"));
        assert!(is_same_origin_request(&config, &headers, true));

        headers.insert(ORIGIN, HeaderValue::from_static("https://evil.example"));
        assert!(!is_same_origin_request(&config, &headers, true));

        let mut sec_fetch = HeaderMap::new();
        sec_fetch.insert(HOST, HeaderValue::from_static("gateway.local"));
        sec_fetch.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(!is_same_origin_request(&config, &sec_fetch, true));

        let mut legacy = HeaderMap::new();
        legacy.insert(HOST, HeaderValue::from_static("gateway.local"));
        legacy.insert(
            REFERER,
            HeaderValue::from_static("https://gateway.local/login"),
        );
        assert!(is_same_origin_request(&config, &legacy, true));

        let missing = HeaderMap::new();
        assert!(!is_same_origin_request(&config, &missing, true));
        assert!(!is_same_origin_request(&config, &missing, false));
    }

    #[test]
    fn extract_client_ip_uses_last_untrusted_hop_from_forwarded_chain() {
        let config = trusted_proxy_config();
        let connect: SocketAddr = "127.0.0.1:12345".parse().expect("socket addr");

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.7, 10.0.0.1"),
        );

        assert_eq!(
            extract_client_ip(&config, &headers, connect),
            "203.0.113.7".parse::<std::net::IpAddr>().expect("ip")
        );
    }

    #[test]
    fn external_origin_overrides_host_for_origin_checks() {
        let config = trusted_proxy_config_with_external_origin();
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("spoofed.internal"));
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("https://wake.example.test"),
        );

        let authority = expected_origin_authority(&config, &headers).expect("authority");
        assert!(is_same_origin_request(&config, &headers, true));
        assert_eq!(authority.host, "wake.example.test");
        assert_eq!(authority.port, Some(443));
    }

    #[test]
    fn websocket_origin_check_uses_same_expected_origin() {
        let config = trusted_proxy_config_with_external_origin();
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("wake.example.test"));
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("https://wake.example.test"),
        );
        assert!(websocket_origin_allowed(&config, &headers));

        headers.insert(ORIGIN, HeaderValue::from_static("https://evil.example"));
        assert!(!websocket_origin_allowed(&config, &headers));
    }

    #[tokio::test]
    async fn gateway_responses_include_security_headers() {
        let response = web_manifest().await;
        assert_eq!(
            response
                .headers()
                .get("content-security-policy")
                .and_then(|value| value.to_str().ok()),
            Some(
                "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
            )
        );
        assert_eq!(
            response
                .headers()
                .get("x-frame-options")
                .and_then(|value| value.to_str().ok()),
            Some("DENY")
        );

        let health = gateway_health().await.into_response();
        assert_eq!(
            health
                .headers()
                .get("x-content-type-options")
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
    }
}
