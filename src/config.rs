use std::collections::HashMap;
use std::env;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Clone)]
pub struct Config {
    pub upstream_host: String,
    pub upstream_port: u16,
    pub health_path: String,
    pub wol_mac: [u8; 6],
    pub wol_broadcast_ip: IpAddr,
    pub wol_broadcast_port: u16,
    pub wake_timeout: Duration,
    pub poll_interval: Duration,
    pub bind_addr: SocketAddr,
    pub password_hash: String,
    pub session_secret: String,
    pub ready_cache_ttl: Duration,
    pub session_ttl: Duration,
    pub cookie_secure: bool,
    pub login_max_failures: usize,
    pub login_lockout: Duration,
    pub login_window: Duration,
    pub edge_wait_timeout: Duration,
    pub trust_proxy_headers: bool,
    pub trusted_proxy_ips: Vec<IpAddr>,
    pub login_max_tracked_ips: usize,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required environment variable: {0}")]
    MissingVar(&'static str),
    #[error("invalid value for {name}: {value}")]
    InvalidValue { name: &'static str, value: String },
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let map = env::vars().collect::<HashMap<String, String>>();
        Self::from_map(&map)
    }

    pub fn from_map(map: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let upstream_host = required(map, "WOL_PROXY_UPSTREAM_HOST")?;
        let upstream_port = parse_required::<u16>(map, "WOL_PROXY_UPSTREAM_PORT")?;
        let health_path = optional(map, "WOL_PROXY_HEALTH_PATH", "/health".to_string());

        if !health_path.starts_with('/') {
            return Err(ConfigError::InvalidValue {
                name: "WOL_PROXY_HEALTH_PATH",
                value: health_path,
            });
        }

        let wol_mac_raw = required(map, "WOL_PROXY_WOL_MAC")?;
        let wol_mac = parse_mac(&wol_mac_raw)?;

        let wol_broadcast_ip = parse_required::<IpAddr>(map, "WOL_PROXY_WOL_BROADCAST_IP")?;
        let wol_broadcast_port = parse_optional::<u16>(map, "WOL_PROXY_WOL_BROADCAST_PORT", 9)?;

        let wake_timeout =
            Duration::from_secs(parse_optional::<u64>(map, "WOL_PROXY_WAKE_TIMEOUT", 180)?);
        let poll_interval =
            Duration::from_secs(parse_optional::<u64>(map, "WOL_PROXY_POLL_INTERVAL", 2)?);

        if wake_timeout.is_zero() || poll_interval.is_zero() || poll_interval > wake_timeout {
            return Err(ConfigError::InvalidValue {
                name: "WOL_PROXY_WAKE_TIMEOUT / WOL_PROXY_POLL_INTERVAL",
                value: format!("wake_timeout={wake_timeout:?}, poll_interval={poll_interval:?}"),
            });
        }

        let bind_addr_raw = optional(map, "WOL_PROXY_BIND_ADDR", "127.0.0.1:8080".to_string());
        let bind_addr =
            bind_addr_raw
                .parse::<SocketAddr>()
                .map_err(|_| ConfigError::InvalidValue {
                    name: "WOL_PROXY_BIND_ADDR",
                    value: bind_addr_raw,
                })?;

        let password_hash = required(map, "WOL_PROXY_PASSWORD_HASH")?;
        let session_secret = required(map, "WOL_PROXY_SESSION_SECRET")?;

        if session_secret.len() < 16 {
            return Err(ConfigError::InvalidValue {
                name: "WOL_PROXY_SESSION_SECRET",
                value: "value must be at least 16 characters".to_string(),
            });
        }

        let ready_cache_ttl =
            Duration::from_secs(parse_optional::<u64>(map, "WOL_PROXY_READY_CACHE_TTL", 5)?);
        let session_ttl = parse_optional_secs(map, "WOL_PROXY_SESSION_TTL_SECS", 43_200)?;
        let cookie_secure = parse_optional::<bool>(map, "WOL_PROXY_COOKIE_SECURE", true)?;
        let login_max_failures = parse_optional::<usize>(map, "WOL_PROXY_LOGIN_MAX_FAILURES", 5)?;
        let login_lockout = parse_optional_secs(map, "WOL_PROXY_LOGIN_LOCKOUT_SECS", 60)?;
        let login_window = Duration::from_secs(parse_optional::<u64>(
            map,
            "WOL_PROXY_LOGIN_WINDOW_SECS",
            300,
        )?);
        let edge_wait_timeout = parse_optional_secs(map, "WOL_PROXY_EDGE_WAIT_TIMEOUT", 20)?;
        let trust_proxy_headers =
            parse_optional::<bool>(map, "WOL_PROXY_TRUST_PROXY_HEADERS", false)?;
        let trusted_proxy_ips = parse_ip_list(map, "WOL_PROXY_TRUSTED_PROXY_IPS")?;
        let login_max_tracked_ips =
            parse_optional::<usize>(map, "WOL_PROXY_LOGIN_MAX_TRACKED_IPS", 10_000)?;

        Ok(Self {
            upstream_host,
            upstream_port,
            health_path,
            wol_mac,
            wol_broadcast_ip,
            wol_broadcast_port,
            wake_timeout,
            poll_interval,
            bind_addr,
            password_hash,
            session_secret,
            ready_cache_ttl,
            session_ttl,
            cookie_secure,
            login_max_failures,
            login_lockout,
            login_window,
            edge_wait_timeout,
            trust_proxy_headers,
            trusted_proxy_ips,
            login_max_tracked_ips,
        })
    }

    pub fn upstream_base_url(&self) -> String {
        format!(
            "http://{}:{}",
            format_host_for_url(&self.upstream_host),
            self.upstream_port
        )
    }
}

pub fn format_host_for_url(host: &str) -> String {
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn required(map: &HashMap<String, String>, key: &'static str) -> Result<String, ConfigError> {
    map.get(key)
        .filter(|v| !v.trim().is_empty())
        .cloned()
        .ok_or(ConfigError::MissingVar(key))
}

fn optional(map: &HashMap<String, String>, key: &'static str, default: String) -> String {
    map.get(key).cloned().unwrap_or(default)
}

fn parse_required<T>(map: &HashMap<String, String>, key: &'static str) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    let raw = required(map, key)?;
    raw.parse::<T>().map_err(|_| ConfigError::InvalidValue {
        name: key,
        value: raw,
    })
}

fn parse_optional<T>(
    map: &HashMap<String, String>,
    key: &'static str,
    default: T,
) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    match map.get(key) {
        Some(raw) => raw.parse::<T>().map_err(|_| ConfigError::InvalidValue {
            name: key,
            value: raw.clone(),
        }),
        None => Ok(default),
    }
}

fn parse_optional_secs(
    map: &HashMap<String, String>,
    key: &'static str,
    default: u64,
) -> Result<Duration, ConfigError> {
    let seconds = parse_optional::<u64>(map, key, default)?;
    Ok(Duration::from_secs(seconds))
}

fn parse_mac(raw: &str) -> Result<[u8; 6], ConfigError> {
    let parts = raw.split(':').collect::<Vec<_>>();
    if parts.len() != 6 {
        return Err(ConfigError::InvalidValue {
            name: "WOL_PROXY_WOL_MAC",
            value: raw.to_string(),
        });
    }

    let mut out = [0_u8; 6];
    for (idx, part) in parts.iter().enumerate() {
        out[idx] = u8::from_str_radix(part, 16).map_err(|_| ConfigError::InvalidValue {
            name: "WOL_PROXY_WOL_MAC",
            value: raw.to_string(),
        })?;
    }

    Ok(out)
}

fn parse_ip_list(
    map: &HashMap<String, String>,
    key: &'static str,
) -> Result<Vec<IpAddr>, ConfigError> {
    let Some(raw) = map.get(key) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }

        let ip = trimmed
            .parse::<IpAddr>()
            .map_err(|_| ConfigError::InvalidValue {
                name: key,
                value: raw.clone(),
            })?;
        out.push(ip);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::env;
    use std::sync::{Mutex, OnceLock};

    use super::{Config, ConfigError};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn valid_env() -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert(
            "WOL_PROXY_UPSTREAM_HOST".to_string(),
            "charon.lan".to_string(),
        );
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
            "$argon2id$v=19$m=19456,t=2,p=1$dGVzdHRlc3R0ZXN0dGVzdA$0xDPfypM3Y76kJumWn95v9PoW7A1WeNseyX2VINeodQ"
                .to_string(),
        );
        map.insert(
            "WOL_PROXY_SESSION_SECRET".to_string(),
            "very-secret-session-key".to_string(),
        );
        map
    }

    #[test]
    fn config_parses_valid_map() {
        let config = Config::from_map(&valid_env()).expect("valid config");
        assert_eq!(config.upstream_host, "charon.lan");
        assert_eq!(config.wake_timeout.as_secs(), 180);
        assert_eq!(config.poll_interval.as_secs(), 2);
    }

    #[test]
    fn config_fails_on_missing_required() {
        let mut env = valid_env();
        env.remove("WOL_PROXY_PASSWORD_HASH");
        assert!(Config::from_map(&env).is_err());
    }

    #[test]
    fn config_fails_on_bad_mac() {
        let mut env = valid_env();
        env.insert("WOL_PROXY_WOL_MAC".to_string(), "not-a-mac".to_string());
        assert!(Config::from_map(&env).is_err());
    }

    #[test]
    fn config_formats_ipv6_base_url() {
        let mut env = valid_env();
        env.insert("WOL_PROXY_UPSTREAM_HOST".to_string(), "fd00::1".to_string());
        let config = Config::from_map(&env).expect("valid config");
        assert_eq!(config.upstream_base_url(), "http://[fd00::1]:3000");
    }

    #[test]
    fn from_env_reads_process_environment() {
        let _guard = env_lock().lock().expect("lock env");

        let pairs = valid_env().into_iter().collect::<Vec<_>>();
        for (k, v) in &pairs {
            unsafe { env::set_var(k, v) };
        }

        let config = Config::from_env().expect("env config");
        assert_eq!(config.upstream_port, 3000);

        for (k, _) in &pairs {
            unsafe { env::remove_var(k) };
        }
    }

    #[test]
    fn config_fails_on_invalid_health_path() {
        let mut env = valid_env();
        env.insert("WOL_PROXY_HEALTH_PATH".to_string(), "health".to_string());
        assert!(matches!(
            Config::from_map(&env),
            Err(ConfigError::InvalidValue {
                name: "WOL_PROXY_HEALTH_PATH",
                ..
            })
        ));
    }

    #[test]
    fn config_fails_on_invalid_timeout_relationship() {
        let mut env = valid_env();
        env.insert("WOL_PROXY_WAKE_TIMEOUT".to_string(), "1".to_string());
        env.insert("WOL_PROXY_POLL_INTERVAL".to_string(), "2".to_string());
        assert!(Config::from_map(&env).is_err());
    }

    #[test]
    fn config_fails_on_invalid_bind_addr() {
        let mut env = valid_env();
        env.insert("WOL_PROXY_BIND_ADDR".to_string(), "not-an-addr".to_string());
        assert!(matches!(
            Config::from_map(&env),
            Err(ConfigError::InvalidValue {
                name: "WOL_PROXY_BIND_ADDR",
                ..
            })
        ));
    }

    #[test]
    fn config_fails_on_short_session_secret() {
        let mut env = valid_env();
        env.insert("WOL_PROXY_SESSION_SECRET".to_string(), "short".to_string());
        assert!(matches!(
            Config::from_map(&env),
            Err(ConfigError::InvalidValue {
                name: "WOL_PROXY_SESSION_SECRET",
                ..
            })
        ));
    }

    #[test]
    fn config_fails_on_invalid_required_parse() {
        let mut env = valid_env();
        env.insert("WOL_PROXY_UPSTREAM_PORT".to_string(), "nope".to_string());
        assert!(matches!(
            Config::from_map(&env),
            Err(ConfigError::InvalidValue {
                name: "WOL_PROXY_UPSTREAM_PORT",
                ..
            })
        ));
    }

    #[test]
    fn config_fails_on_invalid_optional_parse() {
        let mut env = valid_env();
        env.insert(
            "WOL_PROXY_LOGIN_WINDOW_SECS".to_string(),
            "invalid".to_string(),
        );
        assert!(matches!(
            Config::from_map(&env),
            Err(ConfigError::InvalidValue {
                name: "WOL_PROXY_LOGIN_WINDOW_SECS",
                ..
            })
        ));
    }

    #[test]
    fn config_fails_on_invalid_mac_segment() {
        let mut env = valid_env();
        env.insert(
            "WOL_PROXY_WOL_MAC".to_string(),
            "aa:bb:cc:dd:ee:gg".to_string(),
        );
        assert!(Config::from_map(&env).is_err());
    }

    #[test]
    fn config_parses_trusted_proxy_ips_list() {
        let mut env = valid_env();
        env.insert(
            "WOL_PROXY_TRUSTED_PROXY_IPS".to_string(),
            "127.0.0.1, ::1, ,10.0.0.1".to_string(),
        );
        let config = Config::from_map(&env).expect("config");
        assert_eq!(config.trusted_proxy_ips.len(), 3);
    }

    #[test]
    fn config_fails_on_invalid_trusted_proxy_ip() {
        let mut env = valid_env();
        env.insert(
            "WOL_PROXY_TRUSTED_PROXY_IPS".to_string(),
            "127.0.0.1,not-an-ip".to_string(),
        );
        assert!(matches!(
            Config::from_map(&env),
            Err(ConfigError::InvalidValue {
                name: "WOL_PROXY_TRUSTED_PROXY_IPS",
                ..
            })
        ));
    }

    #[test]
    fn config_parses_optional_overrides() {
        let mut env = valid_env();
        env.insert("WOL_PROXY_SESSION_TTL_SECS".to_string(), "120".to_string());
        env.insert("WOL_PROXY_LOGIN_LOCKOUT_SECS".to_string(), "9".to_string());
        env.insert("WOL_PROXY_EDGE_WAIT_TIMEOUT".to_string(), "7".to_string());

        let config = Config::from_map(&env).expect("config");
        assert_eq!(config.session_ttl.as_secs(), 120);
        assert_eq!(config.login_lockout.as_secs(), 9);
        assert_eq!(config.edge_wait_timeout.as_secs(), 7);
    }
}
