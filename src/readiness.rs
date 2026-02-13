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
}

impl SystemReadinessProbe {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let health_url = format!(
            "{}{}",
            config.upstream_base_url(),
            normalize_health_path(&config.health_path)
        );

        let client = Client::builder().timeout(config.poll_interval).build()?;

        Ok(Self {
            host: config.upstream_host.clone(),
            health_url,
            client,
        })
    }
}

#[async_trait]
impl ReadinessProbe for SystemReadinessProbe {
    async fn ping(&self) -> bool {
        match Command::new("ping")
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

fn normalize_health_path(path: &str) -> &str {
    if path.starts_with('/') {
        path
    } else {
        "/health"
    }
}
