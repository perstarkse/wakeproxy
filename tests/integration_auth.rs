use std::sync::Arc;

use axum::http::header::{ACCEPT, COOKIE, LOCATION, SET_COOKIE, USER_AGENT};
use tera::Tera;

mod common;

use common::{
    AlwaysReadyProbe, OkWakeSender, build_config, login_and_extract_cookie, spawn_gateway,
    spawn_gateway_with_templates, spawn_upstream, test_client,
};

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
async fn unauthenticated_ios_document_navigation_uses_soft_redirect_to_login() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-ios-login-redirect");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let response = client
        .get(format!("{}/demo?x=1", gateway.url()))
        .header(
            USER_AGENT,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15",
        )
        .header(ACCEPT, "text/html")
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response.headers().get(LOCATION).is_none());
    let body = response.text().await.expect("body");
    assert!(body.contains("window.location.replace(\"/_auth/login?next=%2Fdemo%3Fx%3D1\")"));
}

#[tokio::test]
async fn login_page_renders_html() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-login-page");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let response = client
        .get(format!("{}/_auth/login?next=%2Fdemo", gateway.url()))
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.expect("body");
    assert!(body.contains("Wake Proxy Login"));
}

#[tokio::test]
async fn gateway_health_endpoint_returns_ok() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-health");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let response = client
        .get(format!("{}/_health", gateway.url()))
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().await.expect("body"), "ok");
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
async fn trusted_proxy_headers_use_forwarded_ip_for_rate_limit() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-trusted-forwarded");
    config.trust_proxy_headers = true;
    config.trusted_proxy_ips = vec!["127.0.0.1".parse().expect("ip")];
    config.login_max_failures = 1;
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let first = client
        .post(format!("{}/_auth/login", gateway.url()))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("x-forwarded-for", "8.8.8.8")
        .body("password=wrong")
        .send()
        .await
        .expect("first");
    assert_eq!(first.status(), reqwest::StatusCode::UNAUTHORIZED);

    let locked = client
        .post(format!("{}/_auth/login", gateway.url()))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("x-forwarded-for", "8.8.8.8")
        .body("password=password-trusted-forwarded")
        .send()
        .await
        .expect("locked");
    assert_eq!(locked.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn malformed_forwarded_ip_falls_back_to_peer_ip() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-forwarded-fallback");
    config.trust_proxy_headers = true;
    config.trusted_proxy_ips = vec!["127.0.0.1".parse().expect("ip")];
    config.login_max_failures = 1;
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let first = client
        .post(format!("{}/_auth/login", gateway.url()))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("x-forwarded-for", "not-an-ip")
        .body("password=wrong")
        .send()
        .await
        .expect("first");
    assert_eq!(first.status(), reqwest::StatusCode::UNAUTHORIZED);

    let locked = client
        .post(format!("{}/_auth/login", gateway.url()))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("x-forwarded-for", "still-not-an-ip")
        .body("password=password-forwarded-fallback")
        .send()
        .await
        .expect("locked");
    assert_eq!(locked.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn login_next_parameter_is_sanitized() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-sanitize");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let response = client
        .post(format!("{}/_auth/login", gateway.url()))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("password=password-sanitize&next=%2F%2Fevil.com")
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/")
    );
}

#[tokio::test]
async fn missing_template_returns_internal_server_error() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-template-missing");
    let templates = Arc::new(Tera::default());
    let gateway =
        spawn_gateway_with_templates(config, OkWakeSender, AlwaysReadyProbe, templates).await;
    let client = test_client();

    let response = client
        .get(format!("{}/_auth/login", gateway.url()))
        .send()
        .await
        .expect("response");

    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        response.text().await.expect("body"),
        "Template rendering failed"
    );
}

#[tokio::test]
async fn unauthenticated_wake_route_redirects_to_login() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-wake-auth");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let response = client
        .get(format!("{}/_wake?next=%2Fdemo", gateway.url()))
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/_auth/login?next=%2F_wake%3Fnext%3D%252Fdemo")
    );
}
