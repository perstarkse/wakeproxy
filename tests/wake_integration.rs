use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use wakeproxy::readiness::ReadinessProbe;
use wakeproxy::wake::{
    EnsureReadyResult, NoopKeepAwakeRunner, WakeConfig, WakeCoordinator, WakePacketSender,
    WakeSendError,
};

#[derive(Clone)]
struct MockWakeSender {
    sends: Arc<AtomicUsize>,
}

#[async_trait]
impl WakePacketSender for MockWakeSender {
    async fn send_magic_packet(&self) -> Result<(), WakeSendError> {
        self.sends.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[derive(Clone)]
struct HttpHealthProbe {
    url: String,
    client: reqwest::Client,
}

#[async_trait]
impl ReadinessProbe for HttpHealthProbe {
    async fn ping(&self) -> bool {
        true
    }

    async fn health(&self) -> bool {
        match self.client.get(&self.url).send().await {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }
}

#[tokio::test]
async fn delayed_upstream_only_triggers_single_wake_cycle() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let upstream_addr = listener.local_addr().expect("local addr");
    let health_url = format!("http://{upstream_addr}/health");

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let app = Router::new().route("/health", get(|| async { "ok" }));
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let sender_count = Arc::new(AtomicUsize::new(0));
    let sender = Arc::new(MockWakeSender {
        sends: sender_count.clone(),
    });
    let probe = Arc::new(HttpHealthProbe {
        url: health_url,
        client: reqwest::Client::new(),
    });

    let coordinator = WakeCoordinator::new(
        WakeConfig {
            wake_timeout: Duration::from_secs(4),
            poll_interval: Duration::from_millis(40),
            ready_cache_ttl: Duration::from_secs(2),
            keep_awake_max: Duration::from_secs(14_400),
        },
        sender,
        probe,
        Arc::new(NoopKeepAwakeRunner),
    );

    let mut tasks = Vec::new();
    for _ in 0..12 {
        let cloned = coordinator.clone();
        tasks.push(tokio::spawn(async move {
            cloned.ensure_ready(Duration::from_secs(2)).await
        }));
    }

    for task in tasks {
        assert!(matches!(
            task.await.expect("join"),
            EnsureReadyResult::Ready { .. }
        ));
    }

    assert_eq!(sender_count.load(Ordering::Relaxed), 1);
    let _ = shutdown_tx.send(());
}
