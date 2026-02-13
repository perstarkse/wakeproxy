use std::net::SocketAddr;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::routing::{any, get};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port = std::env::args()
        .nth(1)
        .as_deref()
        .unwrap_or("6994")
        .parse::<u16>()?;

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .fallback(any(echo_handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;
    println!("mock-upstream listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn echo_handler(request: Request<Body>) -> String {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap_or_default();
    let body_text = String::from_utf8_lossy(&bytes);

    let query = parts.uri.query().unwrap_or("");
    let xff = parts
        .headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let xfp = parts
        .headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let xfh = parts
        .headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    format!(
        "method={};path={};query={};xff={};xfp={};xfh={};body={}",
        parts.method,
        parts.uri.path(),
        query,
        xff,
        xfp,
        xfh,
        body_text
    )
}
