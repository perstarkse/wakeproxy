use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordVerifier};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue};
use cookie::{Cookie, SameSite};
use rand::distr::{Alphanumeric, SampleString};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

use crate::config::Config;

const SESSION_COOKIE_NAME: &str = "wol_session";

#[derive(Clone)]
pub struct AuthService {
    password_hash: String,
    session_secret: String,
    session_ttl: Duration,
    cookie_secure: bool,
    login_max_failures: usize,
    login_lockout: Duration,
    login_window: Duration,
    login_max_tracked_ips: usize,
    sessions: Arc<RwLock<HashMap<String, SessionRecord>>>,
    attempts: Arc<Mutex<HashMap<IpAddr, AttemptRecord>>>,
}

#[derive(Debug, Clone)]
pub enum LoginResult {
    Success { cookie_header: HeaderValue },
    InvalidCredentials,
    Locked { retry_after: Duration },
}

#[derive(Debug)]
struct SessionRecord {
    expires_at: Instant,
}

#[derive(Debug)]
struct AttemptRecord {
    failures: Vec<Instant>,
    locked_until: Option<Instant>,
    last_seen: Instant,
}

impl AttemptRecord {
    fn new(now: Instant) -> Self {
        Self {
            failures: Vec::new(),
            locked_until: None,
            last_seen: now,
        }
    }
}

impl AuthService {
    pub fn new(config: &Config) -> Self {
        Self {
            password_hash: config.password_hash.clone(),
            session_secret: config.session_secret.clone(),
            session_ttl: config.session_ttl,
            cookie_secure: config.cookie_secure,
            login_max_failures: config.login_max_failures,
            login_lockout: config.login_lockout,
            login_window: config.login_window,
            login_max_tracked_ips: config.login_max_tracked_ips,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn login(&self, ip: IpAddr, password: &str) -> LoginResult {
        if let Some(remaining) = self.is_locked(ip).await {
            return LoginResult::Locked {
                retry_after: remaining,
            };
        }

        if !self.verify_password(password) {
            self.register_failure(ip).await;
            return LoginResult::InvalidCredentials;
        }

        self.clear_failures(ip).await;
        let token = self.issue_session_token().await;
        LoginResult::Success {
            cookie_header: self.build_cookie_header(&token),
        }
    }

    pub async fn authenticate_headers(&self, headers: &HeaderMap) -> Option<()> {
        let token = parse_cookie(headers, SESSION_COOKIE_NAME)?;
        let token_hash = hash_token(&self.session_secret, &token);

        let mut sessions = self.sessions.write().await;
        let now = Instant::now();

        sessions.retain(|_, record| record.expires_at > now);

        if let Some(record) = sessions.get_mut(&token_hash) {
            record.expires_at = now + self.session_ttl;
            return Some(());
        }

        None
    }

    pub async fn logout_from_headers(&self, headers: &HeaderMap) {
        if let Some(token) = parse_cookie(headers, SESSION_COOKIE_NAME) {
            let token_hash = hash_token(&self.session_secret, &token);
            self.sessions.write().await.remove(&token_hash);
        }
    }

    pub fn clear_cookie_header(&self) -> HeaderValue {
        let mut cookie = Cookie::new(SESSION_COOKIE_NAME, "");
        cookie.set_path("/");
        cookie.set_http_only(true);
        cookie.set_same_site(SameSite::Lax);
        cookie.set_secure(self.cookie_secure);
        cookie.make_removal();

        HeaderValue::from_str(&cookie.to_string()).expect("cookie header value")
    }

    fn build_cookie_header(&self, token: &str) -> HeaderValue {
        let mut cookie = Cookie::new(SESSION_COOKIE_NAME, token.to_string());
        cookie.set_path("/");
        cookie.set_http_only(true);
        cookie.set_same_site(SameSite::Lax);
        cookie.set_secure(self.cookie_secure);

        HeaderValue::from_str(&cookie.to_string()).expect("cookie header value")
    }

    async fn issue_session_token(&self) -> String {
        let token = Alphanumeric.sample_string(&mut rand::rng(), 48);
        let token_hash = hash_token(&self.session_secret, &token);
        self.sessions.write().await.insert(
            token_hash,
            SessionRecord {
                expires_at: Instant::now() + self.session_ttl,
            },
        );

        token
    }

    async fn is_locked(&self, ip: IpAddr) -> Option<Duration> {
        let mut attempts = self.attempts.lock().await;
        let now = Instant::now();
        // Prune before lookup so stale lockouts cannot keep an IP blocked forever.
        prune_attempts(
            &mut attempts,
            now,
            self.login_window,
            self.login_max_tracked_ips,
        );

        let record = attempts
            .entry(ip)
            .or_insert_with(|| AttemptRecord::new(now));
        record.last_seen = now;

        if let Some(locked_until) = record.locked_until {
            return Some(locked_until - now);
        }

        prune_attempts(
            &mut attempts,
            now,
            self.login_window,
            self.login_max_tracked_ips,
        );

        None
    }

    async fn register_failure(&self, ip: IpAddr) {
        let mut attempts = self.attempts.lock().await;
        let now = Instant::now();
        prune_attempts(
            &mut attempts,
            now,
            self.login_window,
            self.login_max_tracked_ips,
        );

        let record = attempts
            .entry(ip)
            .or_insert_with(|| AttemptRecord::new(now));
        record.last_seen = now;

        record
            .failures
            .retain(|failure| now.duration_since(*failure) <= self.login_window);
        record.failures.push(now);

        if record.failures.len() >= self.login_max_failures {
            record.locked_until = Some(now + self.login_lockout);
            record.failures.clear();
        }

        prune_attempts(
            &mut attempts,
            now,
            self.login_window,
            self.login_max_tracked_ips,
        );
    }

    async fn clear_failures(&self, ip: IpAddr) {
        self.attempts.lock().await.remove(&ip);
    }

    fn verify_password(&self, candidate: &str) -> bool {
        let parsed = match PasswordHash::new(&self.password_hash) {
            Ok(hash) => hash,
            Err(_) => return false,
        };

        Argon2::default()
            .verify_password(candidate.as_bytes(), &parsed)
            .is_ok()
    }
}

pub fn append_set_cookie(headers: &mut HeaderMap, cookie_value: HeaderValue) {
    headers.append(SET_COOKIE, cookie_value);
}

pub fn parse_cookie(headers: &HeaderMap, key: &str) -> Option<String> {
    let cookie_header = headers.get(COOKIE)?.to_str().ok()?;

    cookie_header
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(cookie_key, cookie_value)| {
            if cookie_key == key {
                Some(cookie_value.to_string())
            } else {
                None
            }
        })
}

fn hash_token(secret: &str, token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn prune_attempts(
    attempts: &mut HashMap<IpAddr, AttemptRecord>,
    now: Instant,
    login_window: Duration,
    login_max_tracked_ips: usize,
) {
    // First pass removes stale failures and expired lockouts from each record.
    attempts.retain(|_, record| {
        record
            .failures
            .retain(|failure| now.duration_since(*failure) <= login_window);

        match record.locked_until {
            Some(locked_until) if locked_until > now => true,
            Some(_) => {
                record.locked_until = None;
                !record.failures.is_empty()
            }
            None => !record.failures.is_empty(),
        }
    });

    if attempts.len() <= login_max_tracked_ips {
        return;
    }

    // Hard-cap tracked IPs so attacker-driven cardinality cannot grow memory unbounded.
    let mut entries = attempts
        .iter()
        .map(|(ip, record)| (*ip, record.last_seen))
        .collect::<Vec<_>>();
    entries.sort_by_key(|(_, last_seen)| *last_seen);

    let overflow = attempts.len() - login_max_tracked_ips;
    for (ip, _) in entries.into_iter().take(overflow) {
        attempts.remove(&ip);
    }
}
