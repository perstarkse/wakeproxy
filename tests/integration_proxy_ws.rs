use axum::http::header::COOKIE;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async, tungstenite};

mod common;

use common::{
    AlwaysReadyProbe, OkWakeSender, build_config, login_and_extract_cookie, spawn_gateway,
    spawn_upstream, test_client,
};

#[tokio::test]
async fn unauthenticated_websocket_upgrade_is_rejected_by_auth_guard() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-7");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;

    let ws_url = format!("ws://{}/ws", gateway.addr);
    let error = connect_async(&ws_url)
        .await
        .expect_err("unauthenticated websocket must fail");

    match error {
        tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), http::StatusCode::SEE_OTHER);
        }
        other => panic!("unexpected websocket error: {other:?}"),
    }
}

#[tokio::test]
async fn authenticated_websocket_request_is_proxied_to_upstream() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-8");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-8").await;
    let ws_url = format!("ws://{}/ws", gateway.addr);
    let mut request = ws_url.into_client_request().expect("ws request");
    request
        .headers_mut()
        .insert(COOKIE, cookie.parse().expect("cookie header value"));

    let (mut socket, response) = connect_async(request).await.expect("websocket connect");
    assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);

    socket
        .send(tungstenite::Message::Text("proxy-ws-check".into()))
        .await
        .expect("send websocket frame");

    let message = socket
        .next()
        .await
        .expect("receive frame")
        .expect("frame is ok");

    match message {
        tungstenite::Message::Text(text) => {
            assert_eq!(text, "upstream:proxy-ws-check");
        }
        other => panic!("unexpected websocket message: {other:?}"),
    }

    socket.close(None).await.expect("close websocket");
}

#[tokio::test]
async fn authenticated_websocket_binary_frame_is_proxied_to_upstream() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-9");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-9").await;
    let ws_url = format!("ws://{}/ws", gateway.addr);
    let mut request = ws_url.into_client_request().expect("ws request");
    request
        .headers_mut()
        .insert(COOKIE, cookie.parse().expect("cookie header value"));

    let (mut socket, response) = connect_async(request).await.expect("websocket connect");
    assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);

    let payload = vec![1_u8, 2, 3, 4, 5, 255];
    socket
        .send(tungstenite::Message::Binary(payload.clone().into()))
        .await
        .expect("send websocket binary frame");

    let message = socket
        .next()
        .await
        .expect("receive frame")
        .expect("frame is ok");

    match message {
        tungstenite::Message::Binary(bytes) => {
            assert_eq!(bytes.as_ref(), payload.as_slice());
        }
        other => panic!("unexpected websocket message: {other:?}"),
    }

    socket.close(None).await.expect("close websocket");
}

#[tokio::test]
async fn authenticated_websocket_close_from_upstream_reaches_client() {
    let upstream = spawn_upstream().await;
    let config = build_config(upstream.addr.port(), "password-10");
    let gateway = spawn_gateway(config, OkWakeSender, AlwaysReadyProbe).await;
    let client = test_client();

    let cookie = login_and_extract_cookie(&client, &gateway.url(), "password-10").await;
    let ws_url = format!("ws://{}/ws", gateway.addr);
    let mut request = ws_url.into_client_request().expect("ws request");
    request
        .headers_mut()
        .insert(COOKIE, cookie.parse().expect("cookie header value"));

    let (mut socket, response) = connect_async(request).await.expect("websocket connect");
    assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);

    socket
        .send(tungstenite::Message::Text("close-me".into()))
        .await
        .expect("send close trigger message");

    let message = socket
        .next()
        .await
        .expect("receive frame")
        .expect("frame is ok");

    match message {
        tungstenite::Message::Close(_) => {}
        other => panic!("unexpected websocket message: {other:?}"),
    }
}
