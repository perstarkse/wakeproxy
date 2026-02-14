use async_trait::async_trait;
use reqwest::Client;
use tokio::process::Command;

use crate::config::Config;

#[async_trait]
pub trait ReadinessProbe: Send + Sync {
    async fn ping(&self) -> bool;
    async fn health(&self) -> bool;
}

#[derive(Clone)]
pub struct SystemReadinessProbe {
    host: String,
    health_url: String,
    client: Client,
    ping_command: String,
}

impl SystemReadinessProbe {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let health_url = format!("{}{}", config.upstream_base_url(), config.health_path);

        let client = Client::builder().timeout(config.poll_interval).build()?;

        Ok(Self {
            host: config.upstream_host.clone(),
            health_url,
            client,
            ping_command: "ping".to_string(),
        })
    }

    #[cfg(test)]
    fn with_ping_command(
        host: String,
        health_url: String,
        timeout: std::time::Duration,
        ping_command: String,
    ) -> Self {
        let client = Client::builder().timeout(timeout).build().expect("client");
        Self {
            host,
            health_url,
            client,
            ping_command,
        }
    }
}

#[async_trait]
impl ReadinessProbe for SystemReadinessProbe {
    async fn ping(&self) -> bool {
        match Command::new(&self.ping_command)
            .arg("-c")
            .arg("1")
            .arg("-W")
            .arg("1")
            .arg(&self.host)
            .output()
            .await
        {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }

    async fn health(&self) -> bool {
        match self.client.get(&self.health_url).send().await {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use axum::Router;
    use axum::routing::get;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    use crate::config::Config;

    use super::{ReadinessProbe, SystemReadinessProbe};

    fn config_with_health_path(path: &str) -> Config {
        let mut map = HashMap::new();
        map.insert(
            "WOL_PROXY_UPSTREAM_HOST".to_string(),
            "127.0.0.1".to_string(),
        );
        map.insert("WOL_PROXY_UPSTREAM_PORT".to_string(), "65534".to_string());
        map.insert("WOL_PROXY_HEALTH_PATH".to_string(), path.to_string());
        map.insert(
            "WOL_PROXY_WOL_MAC".to_string(),
            "aa:bb:cc:dd:ee:ff".to_string(),
        );
        map.insert(
            "WOL_PROXY_WOL_BROADCAST_IP".to_string(),
            "127.255.255.255".to_string(),
        );
        map.insert(
            "WOL_PROXY_PASSWORD_HASH".to_string(),
            "$argon2id$v=19$m=19456,t=2,p=1$dGVzdHRlc3R0ZXN0dGVzdA$0xDPfypM3Y76kJumWn95v9PoW7A1WeNseyX2VINeodQ"
                .to_string(),
        );
        map.insert(
            "WOL_PROXY_SESSION_SECRET".to_string(),
            "integration-test-secret".to_string(),
        );
        Config::from_map(&map).expect("config")
    }

    #[tokio::test]
    async fn health_returns_true_for_success_status() {
        let app = Router::new().route("/health", get(|| async { "ok" }));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server_task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        let probe = SystemReadinessProbe::with_ping_command(
            "127.0.0.1".to_string(),
            format!("http://{addr}/health"),
            Duration::from_secs(1),
            "ping".to_string(),
        );
        assert!(probe.health().await);
        let _ = shutdown_tx.send(());
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn health_returns_false_on_connection_error() {
        let probe = SystemReadinessProbe::with_ping_command(
            "127.0.0.1".to_string(),
            "http://127.0.0.1:1/health".to_string(),
            Duration::from_millis(200),
            "ping".to_string(),
        );
        assert!(!probe.health().await);
    }

    #[tokio::test]
    async fn ping_returns_false_when_command_is_missing() {
        // Custom command allows deterministic coverage of command-spawn failures.
        let probe = SystemReadinessProbe::with_ping_command(
            "127.0.0.1".to_string(),
            "http://127.0.0.1/health".to_string(),
            Duration::from_millis(200),
            "command-that-does-not-exist".to_string(),
        );

        assert!(!probe.ping().await);
    }

    #[tokio::test]
    async fn ping_command_invocation_path_is_executed() {
        let probe = SystemReadinessProbe::with_ping_command(
            "127.0.0.1".to_string(),
            "http://127.0.0.1/health".to_string(),
            Duration::from_millis(200),
            "ping".to_string(),
        );

        let _ = probe.ping().await;
    }

    #[test]
    fn new_uses_configured_health_path() {
        let config = config_with_health_path("/healthz");
        let probe = SystemReadinessProbe::new(&config).expect("probe");
        assert!(probe.health_url.ends_with("/healthz"));
    }
}
