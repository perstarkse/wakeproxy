use std::time::Duration;

use axum::http::header::COOKIE;

mod common;

use common::{
    AlwaysReadyProbe, FailingWakeSender, NeverReadyProbe, OkWakeSender, build_config,
    login_and_extract_cookie, spawn_gateway, spawn_upstream, test_client,
};

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
async fn edge_timeout_returns_still_waking_template() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-still-waking");
    config.wake_timeout = Duration::from_secs(2);
    config.poll_interval = Duration::from_millis(100);
    config.edge_wait_timeout = Duration::from_millis(10);

    let gateway = spawn_gateway(config, OkWakeSender, NeverReadyProbe).await;
    let client = test_client();
    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-still-waking").await;

    let response = client
        .get(format!("{}/demo", gateway.url()))
        .header(COOKIE, cookie)
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let body = response.text().await.expect("body");
    assert!(body.contains("still waking"));
}
