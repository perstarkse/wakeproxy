use std::net::IpAddr;

use axum::body::Body;
use axum::http::header::{CONNECTION, HOST, UPGRADE};
use axum::http::{HeaderMap, HeaderValue, Request, Response, Uri};
use hyper::StatusCode;
use hyper::body::Incoming;
use hyper::upgrade;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::config::Config;
use crate::config::format_host_for_url;

#[derive(Clone)]
pub struct ProxyService {
    config: std::sync::Arc<Config>,
    client: Client<HttpConnector, Body>,
}

impl ProxyService {
    pub fn new(config: std::sync::Arc<Config>, client: Client<HttpConnector, Body>) -> Self {
        Self { config, client }
    }

    pub async fn forward(
        &self,
        mut req: Request<Body>,
        client_ip: IpAddr,
    ) -> anyhow::Result<Response<Body>> {
        let is_upgrade = is_upgrade_request(req.headers());
        let on_client_upgrade = if is_upgrade {
            Some(upgrade::on(&mut req))
        } else {
            None
        };

        let proxied_uri = build_upstream_uri(&self.config, req.uri())?;
        *req.uri_mut() = proxied_uri;
        apply_forwarded_headers(req.headers_mut(), client_ip);
        req.headers_mut().insert(
            HOST,
            HeaderValue::from_str(&self.config.upstream_host)
                .unwrap_or(HeaderValue::from_static("upstream")),
        );

        let mut upstream_response: Response<Incoming> = self.client.request(req).await?;

        if is_upgrade
            && upstream_response.status() == StatusCode::SWITCHING_PROTOCOLS
            && let Some(on_client_upgrade) = on_client_upgrade
        {
            // Both upgraded sockets are tunneled in the background so this
            // request can return the 101 response immediately.
            let on_upstream_upgrade = upgrade::on(&mut upstream_response);
            tokio::spawn(bridge_upgrade_streams(
                async move { on_client_upgrade.await.map(TokioIo::new) },
                async move { on_upstream_upgrade.await.map(TokioIo::new) },
            ));
        }

        let status = upstream_response.status();
        let headers = upstream_response.headers().clone();
        let mut response = Response::new(Body::new(upstream_response.into_body()));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        Ok(response)
    }
}

fn build_upstream_uri(config: &Config, incoming_uri: &Uri) -> anyhow::Result<Uri> {
    let path_query = incoming_uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let as_string = format!(
        "http://{}:{}{}",
        format_host_for_url(&config.upstream_host),
        config.upstream_port,
        path_query
    );
    Ok(as_string.parse::<Uri>()?)
}

fn apply_forwarded_headers(headers: &mut HeaderMap, client_ip: IpAddr) {
    if let Ok(value) = HeaderValue::from_str(&client_ip.to_string()) {
        headers.insert("x-forwarded-for", value);
    }

    if !headers.contains_key("x-forwarded-proto") {
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
    }

    if let Some(host) = headers.get(HOST).cloned() {
        headers.insert("x-forwarded-host", host);
    }
}

fn is_upgrade_request(headers: &HeaderMap) -> bool {
    headers.contains_key(UPGRADE)
        && headers
            .get(CONNECTION)
            .and_then(|value| value.to_str().ok())
            .map(|connection| connection.to_ascii_lowercase().contains("upgrade"))
            .unwrap_or(false)
}

async fn bridge_upgrade_streams<C, U, CS, US, CE, UE>(client_upgrade: C, upstream_upgrade: U)
where
    C: std::future::Future<Output = Result<CS, CE>>,
    U: std::future::Future<Output = Result<US, UE>>,
    CS: AsyncRead + AsyncWrite + Unpin,
    US: AsyncRead + AsyncWrite + Unpin,
{
    match (client_upgrade.await, upstream_upgrade.await) {
        (Ok(client_upgraded), Ok(upstream_upgraded)) => {
            let mut client_upgraded = client_upgraded;
            let mut upstream_upgraded = upstream_upgraded;
            let _ =
                tokio::io::copy_bidirectional(&mut client_upgraded, &mut upstream_upgraded).await;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    use axum::body::Body;
    use axum::http::header::HOST;
    use axum::http::{HeaderMap, HeaderValue, Request, Uri};
    use hyper_util::client::legacy::Client;
    use hyper_util::client::legacy::connect::HttpConnector;
    use hyper_util::rt::TokioExecutor;

    use crate::config::Config;

    use super::apply_forwarded_headers;
    use super::build_upstream_uri;
    use super::ProxyService;
    use super::bridge_upgrade_streams;

    fn build_config_with_host(host: &str) -> Config {
        let mut map = HashMap::new();
        map.insert("WOL_PROXY_UPSTREAM_HOST".to_string(), host.to_string());
        map.insert("WOL_PROXY_UPSTREAM_PORT".to_string(), "3000".to_string());
        map.insert(
            "WOL_PROXY_WOL_MAC".to_string(),
            "aa:bb:cc:dd:ee:ff".to_string(),
        );
        map.insert(
            "WOL_PROXY_WOL_BROADCAST_IP".to_string(),
            "10.0.0.255".to_string(),
        );
        map.insert("WOL_PROXY_PASSWORD_HASH".to_string(), "hash".to_string());
        map.insert(
            "WOL_PROXY_SESSION_SECRET".to_string(),
            "super-secret-session-key".to_string(),
        );

        Config::from_map(&map).expect("config")
    }

    #[tokio::test]
    async fn covers_upgrade_error_branch() {
        // The coverage target here is the warning/error branch, not assertions.
        bridge_upgrade_streams::<_, _, tokio::io::DuplexStream, tokio::io::DuplexStream, (), ()>(
            async { Err(()) },
            async { Err(()) },
        )
        .await;
    }

    #[test]
    fn build_upstream_uri_returns_error_for_invalid_host() {
        let config = build_config_with_host("[");
        let uri: Uri = "/demo?x=1".parse().expect("uri");

        assert!(build_upstream_uri(&config, &uri).is_err());
    }

    #[test]
    fn apply_forwarded_headers_preserves_existing_proto_and_missing_host() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-proto",
            HeaderValue::from_static("http"),
        );

        apply_forwarded_headers(&mut headers, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));

        assert_eq!(
            headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok()),
            Some("http")
        );
        assert!(headers.get("x-forwarded-host").is_none());
    }

    #[test]
    fn apply_forwarded_headers_copies_host_when_present() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("example.test"));

        apply_forwarded_headers(&mut headers, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)));

        assert_eq!(
            headers
                .get("x-forwarded-host")
                .and_then(|v| v.to_str().ok()),
            Some("example.test")
        );
        assert_eq!(
            headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok()),
            Some("https")
        );
    }

    #[tokio::test]
    async fn forward_returns_error_when_upstream_uri_cannot_be_built() {
        let config = std::sync::Arc::new(build_config_with_host("["));
        let client: Client<HttpConnector, Body> =
            Client::builder(TokioExecutor::new()).build(HttpConnector::new());
        let service = ProxyService::new(config, client);

        let request = Request::builder()
            .uri("/demo")
            .body(Body::empty())
            .expect("request");

        assert!(
            service
                .forward(request, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10)))
                .await
                .is_err()
        );
    }
}
