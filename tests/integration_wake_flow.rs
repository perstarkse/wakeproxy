use std::fs;
use std::time::Duration;

use axum::http::header::{ACCEPT, COOKIE, LOCATION, USER_AGENT};

mod common;

use common::{
    AlwaysReadyProbe, FailingWakeSender, NeverReadyProbe, OkWakeSender, add_same_origin_headers,
    build_config, login_and_extract_cookie, spawn_gateway, spawn_upstream, test_client,
};

#[tokio::test]
async fn sleeping_upstream_redirects_authenticated_requests_to_wake_page() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-4");
    config.wake_poll_interval_ms = 20;
    let gateway = spawn_gateway(config, OkWakeSender, NeverReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-4").await;

    let response = client
        .get(format!("{}/demo?x=1", gateway.url()))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("response");

    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/_wake?next=%2Fdemo%3Fx%3D1")
    );
}

#[tokio::test]
async fn wake_page_renders_status_when_upstream_is_sleeping() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-wake-page");
    config.wake_poll_interval_ms = 20;
    let gateway = spawn_gateway(config, OkWakeSender, NeverReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-wake-page").await;

    let response = client
        .get(format!("{}/_wake?next=%2Fdemo", gateway.url()))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("wake page response");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store, no-cache, must-revalidate")
    );
    let body = response.text().await.expect("body");
    assert!(body.contains("Remote system status"));
    assert!(body.contains("data-wake-state=\"sleeping\""));
    assert!(body.contains("data-state-label"));
    assert!(body.contains("history.replaceState"));
    assert!(body.contains("window.location.reload()"));
    assert!(body.contains("Wake and connect"));
    assert!(body.contains("name=\"keep_awake_for_secs\""));
    assert!(body.contains("Defaults to 15 minutes"));
    assert!(body.contains("value=\"900\" selected"));
    assert!(body.contains("15 minutes"));
    assert!(body.contains("30 minutes"));
    assert!(body.contains("1 hour"));
}

#[tokio::test]
async fn wake_sender_failure_surfaces_failed_state_in_polling_api() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-5");
    config.wake_poll_interval_ms = 20;
    let gateway = spawn_gateway(config, FailingWakeSender, NeverReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-5").await;

    let start = add_same_origin_headers(
        client
            .post(format!("{}/_wake/start", gateway.url()))
            .header(COOKIE, &cookie)
            .header("content-type", "application/x-www-form-urlencoded"),
        &gateway.url(),
    )
    .body("next=%2Fdemo&keep_awake_for_secs=900")
    .send()
    .await
    .expect("wake start response");
    assert_eq!(start.status(), reqwest::StatusCode::SEE_OTHER);

    tokio::time::sleep(Duration::from_millis(30)).await;

    let poll = client
        .get(format!("{}/_wake/poll?next=%2Fdemo", gateway.url()))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("poll response");

    assert_eq!(poll.status(), reqwest::StatusCode::OK);
    let body = poll.text().await.expect("poll body");
    assert!(body.contains("\"state\":\"failed\""));
    assert!(body.contains("\"ready\":false"));
    assert!(body.contains("\"retryable\":true"));
    assert!(body.contains("simulated wake sender failure"));
}

#[tokio::test]
async fn wake_timeout_surfaces_timed_out_state_in_polling_api() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-timeout");
    config.wake_timeout = Duration::from_millis(120);
    config.poll_interval = Duration::from_millis(30);
    config.wake_poll_interval_ms = 20;

    let gateway = spawn_gateway(config, OkWakeSender, NeverReadyProbe).await;
    let client = test_client();
    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-timeout").await;

    let start = add_same_origin_headers(
        client
            .post(format!("{}/_wake/start", gateway.url()))
            .header(COOKIE, &cookie)
            .header("content-type", "application/x-www-form-urlencoded"),
        &gateway.url(),
    )
    .body("next=%2Fdemo&keep_awake_for_secs=900")
    .send()
    .await
    .expect("wake start response");
    assert_eq!(start.status(), reqwest::StatusCode::SEE_OTHER);

    tokio::time::sleep(Duration::from_millis(220)).await;

    let poll = client
        .get(format!("{}/_wake/poll?next=%2Fdemo", gateway.url()))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("poll response");

    assert_eq!(poll.status(), reqwest::StatusCode::OK);
    let body = poll.text().await.expect("poll body");
    assert!(body.contains("\"state\":\"timed_out\""));
    assert!(body.contains("\"ready\":false"));
    assert!(body.contains("\"retryable\":true"));
}

#[tokio::test]
async fn wake_page_redirects_to_target_when_upstream_is_already_ready() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-6");
    config.wake_poll_interval_ms = 20;
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-6").await;

    let response = client
        .get(format!("{}/_wake?next=%2Fdemo", gateway.url()))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("wake page response");

    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/demo")
    );
}

#[tokio::test]
async fn wake_page_ready_on_ios_document_navigation_uses_soft_redirect() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-ios-wake-redirect");
    config.wake_poll_interval_ms = 20;
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie =
        login_and_extract_cookie(&client, &gateway.url(), "password-ios-wake-redirect").await;

    let response = client
        .get(format!("{}/_wake?next=%2Fdemo", gateway.url()))
        .header(COOKIE, &cookie)
        .header(
            USER_AGENT,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15",
        )
        .header(ACCEPT, "text/html")
        .send()
        .await
        .expect("wake page response");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response.headers().get(LOCATION).is_none());
    let body = response.text().await.expect("body");
    assert!(body.contains("window.location.replace(\"/demo\")"));
}

#[tokio::test]
async fn wake_start_with_keep_awake_failure_still_redirects_to_wake_page() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-keep-awake-fail");
    config.wake_poll_interval_ms = 20;

    let temp_dir = std::env::temp_dir().join(format!(
        "wakeproxy-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("temp dir");
    let command_path = temp_dir.join("keep-awake.sh");
    fs::write(&command_path, "#!/bin/sh\necho boom >&2\nexit 1\n").expect("script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&command_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&command_path, permissions).expect("chmod");
    }
    config.keep_awake_command = Some(command_path);

    let gateway = spawn_gateway(config, OkWakeSender, NeverReadyProbe).await;
    let client = test_client();
    let cookie =
        login_and_extract_cookie(&client, &gateway.url(), "password-keep-awake-fail").await;

    let start = add_same_origin_headers(
        client
            .post(format!("{}/_wake/start", gateway.url()))
            .header(COOKIE, &cookie)
            .header("content-type", "application/x-www-form-urlencoded"),
        &gateway.url(),
    )
    .body("next=%2Fdemo&keep_awake_for_secs=900")
    .send()
    .await
    .expect("wake start response");

    assert_eq!(start.status(), reqwest::StatusCode::SEE_OTHER);
    assert_eq!(
        start
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/_wake?next=%2Fdemo")
    );
}

#[tokio::test]
async fn wake_start_redirects_directly_when_upstream_is_already_ready() {
    let upstream = spawn_upstream().await;
    let mut config = build_config(upstream.addr.port(), "password-ready-start");
    config.wake_poll_interval_ms = 20;
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-ready-start").await;

    let first_start = add_same_origin_headers(
        client
            .post(format!("{}/_wake/start", gateway.url()))
            .header(COOKIE, &cookie)
            .header("content-type", "application/x-www-form-urlencoded"),
        &gateway.url(),
    )
    .body("next=%2Fdemo&keep_awake_for_secs=900")
    .send()
    .await
    .expect("first wake start response");
    assert_eq!(first_start.status(), reqwest::StatusCode::SEE_OTHER);

    tokio::time::sleep(Duration::from_millis(60)).await;

    let response = add_same_origin_headers(
        client
            .post(format!("{}/_wake/start", gateway.url()))
            .header(COOKIE, &cookie)
            .header("content-type", "application/x-www-form-urlencoded"),
        &gateway.url(),
    )
    .body("next=%2Fdemo&keep_awake_for_secs=900")
    .send()
    .await
    .expect("wake start response");

    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/demo")
    );
}
