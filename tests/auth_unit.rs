use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHasher, SaltString};
use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use wol_web_proxy::auth::{AuthService, LoginResult, parse_cookie};
use wol_web_proxy::config::Config;

fn build_config(password: &str) -> Config {
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("hash")
        .to_string();

    let mut map = HashMap::new();
    map.insert("WOL_PROXY_UPSTREAM_HOST".to_string(), "charon".to_string());
    map.insert("WOL_PROXY_UPSTREAM_PORT".to_string(), "3000".to_string());
    map.insert(
        "WOL_PROXY_WOL_MAC".to_string(),
        "aa:bb:cc:dd:ee:ff".to_string(),
    );
    map.insert(
        "WOL_PROXY_WOL_BROADCAST_IP".to_string(),
        "10.0.0.255".to_string(),
    );
    map.insert("WOL_PROXY_PASSWORD_HASH".to_string(), password_hash);
    map.insert(
        "WOL_PROXY_SESSION_SECRET".to_string(),
        "super-secret-session-key".to_string(),
    );
    map.insert("WOL_PROXY_LOGIN_MAX_FAILURES".to_string(), "2".to_string());
    map.insert("WOL_PROXY_LOGIN_LOCKOUT_SECS".to_string(), "30".to_string());

    Config::from_map(&map).expect("config")
}

#[tokio::test]
async fn locks_after_failed_attempts() {
    let auth = AuthService::new(&build_config("correct-password"));
    let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));

    let first = auth.login(ip, "wrong").await;
    assert!(matches!(first, LoginResult::InvalidCredentials));

    let second = auth.login(ip, "wrong").await;
    assert!(matches!(second, LoginResult::InvalidCredentials));

    let third = auth.login(ip, "correct-password").await;
    assert!(matches!(third, LoginResult::Locked { .. }));
}

#[tokio::test]
async fn malformed_hash_is_rejected_by_config() {
    let mut map = HashMap::new();
    map.insert("WOL_PROXY_UPSTREAM_HOST".to_string(), "charon".to_string());
    map.insert("WOL_PROXY_UPSTREAM_PORT".to_string(), "3000".to_string());
    map.insert(
        "WOL_PROXY_WOL_MAC".to_string(),
        "aa:bb:cc:dd:ee:ff".to_string(),
    );
    map.insert(
        "WOL_PROXY_WOL_BROADCAST_IP".to_string(),
        "10.0.0.255".to_string(),
    );
    map.insert(
        "WOL_PROXY_PASSWORD_HASH".to_string(),
        "not-a-hash".to_string(),
    );
    map.insert(
        "WOL_PROXY_SESSION_SECRET".to_string(),
        "super-secret-session-key".to_string(),
    );

    assert!(Config::from_map(&map).is_err());
}

#[tokio::test]
async fn expired_lockout_is_cleared_on_next_attempt() {
    let mut map = HashMap::new();
    map.insert("WOL_PROXY_UPSTREAM_HOST".to_string(), "charon".to_string());
    map.insert("WOL_PROXY_UPSTREAM_PORT".to_string(), "3000".to_string());
    map.insert(
        "WOL_PROXY_WOL_MAC".to_string(),
        "aa:bb:cc:dd:ee:ff".to_string(),
    );
    map.insert(
        "WOL_PROXY_WOL_BROADCAST_IP".to_string(),
        "10.0.0.255".to_string(),
    );

    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password("correct".as_bytes(), &salt)
        .expect("hash")
        .to_string();
    map.insert("WOL_PROXY_PASSWORD_HASH".to_string(), password_hash);
    map.insert(
        "WOL_PROXY_SESSION_SECRET".to_string(),
        "super-secret-session-key".to_string(),
    );
    map.insert("WOL_PROXY_LOGIN_MAX_FAILURES".to_string(), "1".to_string());
    map.insert("WOL_PROXY_LOGIN_LOCKOUT_SECS".to_string(), "0".to_string());

    let config = Config::from_map(&map).expect("config");
    let auth = AuthService::new(&config);
    let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 77));

    let _ = auth.login(ip, "wrong").await;
    // Zero-second lockout covers the stale lock cleanup path immediately.
    assert!(matches!(
        auth.login(ip, "correct").await,
        LoginResult::Success { .. }
    ));
}

#[tokio::test]
async fn active_lockout_returns_retry_after_then_expires() {
    let mut map = HashMap::new();
    map.insert("WOL_PROXY_UPSTREAM_HOST".to_string(), "charon".to_string());
    map.insert("WOL_PROXY_UPSTREAM_PORT".to_string(), "3000".to_string());
    map.insert(
        "WOL_PROXY_WOL_MAC".to_string(),
        "aa:bb:cc:dd:ee:ff".to_string(),
    );
    map.insert(
        "WOL_PROXY_WOL_BROADCAST_IP".to_string(),
        "10.0.0.255".to_string(),
    );

    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password("correct".as_bytes(), &salt)
        .expect("hash")
        .to_string();
    map.insert("WOL_PROXY_PASSWORD_HASH".to_string(), password_hash);
    map.insert(
        "WOL_PROXY_SESSION_SECRET".to_string(),
        "super-secret-session-key".to_string(),
    );
    map.insert("WOL_PROXY_LOGIN_MAX_FAILURES".to_string(), "1".to_string());
    map.insert("WOL_PROXY_LOGIN_LOCKOUT_SECS".to_string(), "1".to_string());

    let config = Config::from_map(&map).expect("config");
    let auth = AuthService::new(&config);
    let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 88));

    let _ = auth.login(ip, "wrong").await;

    assert!(matches!(
        auth.login(ip, "correct").await,
        LoginResult::Locked { retry_after } if retry_after > Duration::from_millis(0)
    ));

    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(matches!(
        auth.login(ip, "correct").await,
        LoginResult::Success { .. }
    ));
}

#[tokio::test]
async fn parse_cookie_returns_none_for_non_matching_key() {
    let mut headers = HeaderMap::new();
    headers.insert(COOKIE, "foo=bar; baz=qux".parse().expect("cookie"));
    assert!(parse_cookie(&headers, "wol_session").is_none());
}

#[tokio::test]
async fn logout_without_session_cookie_is_noop() {
    let auth = AuthService::new(&build_config("correct-password"));
    let headers = HeaderMap::new();
    auth.logout_from_headers(&headers).await;
}

#[tokio::test]
async fn many_ips_exercise_attempt_pruning_path() {
    let mut map = HashMap::new();
    map.insert("WOL_PROXY_UPSTREAM_HOST".to_string(), "charon".to_string());
    map.insert("WOL_PROXY_UPSTREAM_PORT".to_string(), "3000".to_string());
    map.insert(
        "WOL_PROXY_WOL_MAC".to_string(),
        "aa:bb:cc:dd:ee:ff".to_string(),
    );
    map.insert(
        "WOL_PROXY_WOL_BROADCAST_IP".to_string(),
        "10.0.0.255".to_string(),
    );

    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password("correct".as_bytes(), &salt)
        .expect("hash")
        .to_string();
    map.insert("WOL_PROXY_PASSWORD_HASH".to_string(), password_hash);
    map.insert(
        "WOL_PROXY_SESSION_SECRET".to_string(),
        "super-secret-session-key".to_string(),
    );
    map.insert(
        "WOL_PROXY_LOGIN_MAX_TRACKED_IPS".to_string(),
        "2".to_string(),
    );

    let config = Config::from_map(&map).expect("config");
    let auth = AuthService::new(&config);

    for idx in 1..=8 {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, idx));
        let _ = auth.login(ip, "wrong").await;
    }
}
