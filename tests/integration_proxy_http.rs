use std::sync::Arc;
use std::time::Duration;

use axum::http::header::COOKIE;
use tera::Tera;
use tokio::net::TcpListener;

mod common;

use common::{
    AlwaysReadyProbe, OkWakeSender, build_config, login_and_extract_cookie, spawn_gateway,
    spawn_gateway_with_templates, spawn_upstream, test_client,
};

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
    assert!(body.contains("xfp=http"));
    assert!(body.contains("xfh=127.0.0.1:"));
}

#[tokio::test]
async fn proxy_connect_error_returns_error_template() {
    let unused_port = {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    };

    let config = build_config(unused_port, "password-proxy-error");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();
    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-proxy-error").await;

    let response = client
        .get(format!("{}/demo", gateway.url()))
        .header(COOKIE, cookie)
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    assert!(
        response
            .text()
            .await
            .expect("body")
            .contains("Gateway error")
    );
}

#[tokio::test]
async fn proxy_connect_error_with_missing_error_template_returns_plain_500() {
    let unused_port = {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    };

    let config = build_config(unused_port, "password-proxy-missing-template");
    let templates = Arc::new(Tera::default());
    let gateway =
        spawn_gateway_with_templates(config, OkWakeSender, AlwaysReadyProbe, templates).await;
    let client = test_client();
    let cookie =
        login_and_extract_cookie(&client, &gateway.url(), "password-proxy-missing-template").await;

    let response = client
        .get(format!("{}/demo", gateway.url()))
        .header(COOKIE, cookie)
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
async fn upgrade_with_non_upgrading_client_hits_upgrade_error_path() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-force-101");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();
    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-force-101").await;

    let response = client
        .get(format!("{}/force-101", gateway.url()))
        .header(COOKIE, cookie)
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::SWITCHING_PROTOCOLS);
    tokio::time::sleep(Duration::from_millis(50)).await;
}
