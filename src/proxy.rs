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
use tracing::warn;

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

        if is_upgrade && upstream_response.status() == StatusCode::SWITCHING_PROTOCOLS {
            if let Some(on_client_upgrade) = on_client_upgrade {
                let on_upstream_upgrade = upgrade::on(&mut upstream_response);
                tokio::spawn(async move {
                    match (on_client_upgrade.await, on_upstream_upgrade.await) {
                        (Ok(client_upgraded), Ok(upstream_upgraded)) => {
                            let mut client_upgraded = TokioIo::new(client_upgraded);
                            let mut upstream_upgraded = TokioIo::new(upstream_upgraded);
                            let _ = tokio::io::copy_bidirectional(
                                &mut client_upgraded,
                                &mut upstream_upgraded,
                            )
                            .await;
                        }
                        _ => {
                            warn!("failed websocket tunnel upgrade");
                        }
                    }
                });
            }
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
