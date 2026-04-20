use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;
use axum::http::header::{HOST, ORIGIN, REFERER};
use axum::response::IntoResponse;

use crate::config::Config;
use crate::http::mark_uncacheable;

#[derive(Debug, Clone)]
pub(crate) struct OriginAuthority {
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
}

pub(crate) fn extract_client_ip(
    config: &Config,
    headers: &HeaderMap,
    connect_info: SocketAddr,
) -> IpAddr {
    if !should_trust_proxy(config, connect_info) {
        return connect_info.ip();
    }

    let peer_ip = connect_info.ip();
    let mut chain = forwarded_for_chain(headers);
    chain.push(peer_ip);

    for ip in chain.into_iter().rev() {
        if !config.trusted_proxy_ips.contains(&ip) {
            return ip;
        }
    }

    peer_ip
}

pub(crate) fn should_trust_proxy(config: &Config, connect_info: SocketAddr) -> bool {
    config.trust_proxy_headers && config.trusted_proxy_ips.contains(&connect_info.ip())
}

fn forwarded_for_chain(headers: &HeaderMap) -> Vec<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|part| part.trim().parse::<IpAddr>().ok())
        .collect()
}

pub(crate) fn csrf_forbidden_response() -> axum::response::Response {
    let mut response = (axum::http::StatusCode::FORBIDDEN, "Forbidden").into_response();
    mark_uncacheable(response.headers_mut());
    response
}

pub(crate) fn expected_origin_authority(
    config: &Config,
    headers: &HeaderMap,
) -> Option<OriginAuthority> {
    if let Some(origin) = config.external_origin.as_deref() {
        return parse_origin_authority(origin);
    }

    let host = headers.get(HOST).and_then(|value| value.to_str().ok())?;
    parse_host_authority(host)
}

pub(crate) fn is_same_origin_request(
    config: &Config,
    headers: &HeaderMap,
    require_explicit_origin: bool,
) -> bool {
    if let Some(site) = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase())
        && site == "cross-site"
    {
        return false;
    }

    if require_explicit_origin && headers.get(ORIGIN).is_none() && headers.get(REFERER).is_none() {
        return false;
    }

    let Some(authority) = expected_origin_authority(config, headers) else {
        return false;
    };

    if let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) {
        return matches_authority(origin, &authority);
    }

    if let Some(referer) = headers.get(REFERER).and_then(|value| value.to_str().ok()) {
        return matches_authority(referer, &authority);
    }

    !require_explicit_origin
}

pub(crate) fn websocket_origin_allowed(config: &Config, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        return true;
    };
    let Some(authority) = expected_origin_authority(config, headers) else {
        return false;
    };
    matches_authority(origin, &authority)
}

fn parse_host_authority(value: &str) -> Option<OriginAuthority> {
    let authority = value.parse::<axum::http::uri::Authority>().ok()?;
    Some(OriginAuthority {
        host: authority
            .host()
            .trim_matches(&['[', ']'][..])
            .to_ascii_lowercase(),
        port: authority.port_u16(),
    })
}

fn parse_origin_authority(value: &str) -> Option<OriginAuthority> {
    let uri = value.parse::<axum::http::Uri>().ok()?;
    let authority = uri.authority()?;
    Some(OriginAuthority {
        host: authority
            .host()
            .trim_matches(&['[', ']'][..])
            .to_ascii_lowercase(),
        port: authority.port_u16().or_else(|| match uri.scheme_str() {
            Some("http") => Some(80),
            Some("https") => Some(443),
            _ => None,
        }),
    })
}

fn matches_authority(value: &str, expected: &OriginAuthority) -> bool {
    let uri = value.parse::<axum::http::Uri>().ok();
    let Some(uri) = uri else {
        return false;
    };
    let Some(authority) = uri.authority() else {
        return false;
    };

    let origin_host = authority
        .host()
        .trim_matches(&['[', ']'][..])
        .to_ascii_lowercase();
    if origin_host != expected.host {
        return false;
    }

    let origin_port = authority.port_u16().or_else(|| match uri.scheme_str() {
        Some("http") => Some(80),
        Some("https") => Some(443),
        _ => None,
    });

    match expected.port {
        Some(port) => origin_port == Some(port),
        None => origin_port.is_none() || origin_port == Some(80) || origin_port == Some(443),
    }
}

pub(crate) fn sanitize_next(next: Option<&str>) -> Option<String> {
    let next = next?;

    if next.starts_with('/') && !next.starts_with("//") {
        Some(next.to_string())
    } else {
        None
    }
}
