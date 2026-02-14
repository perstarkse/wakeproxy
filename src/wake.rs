use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, watch};
use tracing::{error, info};

use crate::readiness::ReadinessProbe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeState {
    Sleeping,
    Waking,
    PingReady,
    HttpReady,
    Ready,
    TimedOut,
    Failed,
}

#[derive(Debug, Clone)]
pub enum EnsureReadyResult {
    Ready { cycle_id: u64 },
    StillWaking { cycle_id: u64, state: WakeState },
    TimedOut { cycle_id: u64 },
    Failed { cycle_id: u64, reason: String },
}

#[derive(Debug, Clone)]
enum WakeOutcome {
    Ready,
    TimedOut,
    Failed(String),
}

#[derive(Debug, Clone, Copy)]
pub struct WakeConfig {
    pub wake_timeout: Duration,
    pub poll_interval: Duration,
    pub ready_cache_ttl: Duration,
}

#[async_trait]
pub trait WakePacketSender: Send + Sync {
    async fn send_magic_packet(&self) -> anyhow::Result<()>;
}

#[derive(Clone)]
pub struct UdpWakeSender {
    mac: [u8; 6],
    broadcast_ip: Ipv4Addr,
    broadcast_port: u16,
}

impl UdpWakeSender {
    pub fn new(mac: [u8; 6], broadcast_ip: Ipv4Addr, broadcast_port: u16) -> Self {
        Self {
            mac,
            broadcast_ip,
            broadcast_port,
        }
    }
}

#[async_trait]
impl WakePacketSender for UdpWakeSender {
    async fn send_magic_packet(&self) -> anyhow::Result<()> {
        let target = SocketAddr::new(IpAddr::V4(self.broadcast_ip), self.broadcast_port);
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.set_broadcast(true)?;
        socket
            .send_to(&build_magic_packet(self.mac), target)
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct WakeCoordinator {
    config: WakeConfig,
    sender: Arc<dyn WakePacketSender>,
    probe: Arc<dyn ReadinessProbe>,
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    next_cycle_id: u64,
    current_cycle: Option<Arc<WakeCycle>>,
    last_ready_at: Option<Instant>,
    last_ready_cycle_id: u64,
}

struct WakeCycle {
    id: u64,
    state_tx: watch::Sender<WakeState>,
    result_tx: watch::Sender<Option<WakeOutcome>>,
}

impl WakeCycle {
    fn set_state(&self, state: WakeState) {
        self.state_tx.send_replace(state);
    }

    fn set_result(&self, outcome: WakeOutcome) {
        self.result_tx.send_replace(Some(outcome));
    }
}

impl WakeCoordinator {
    pub fn new(
        config: WakeConfig,
        sender: Arc<dyn WakePacketSender>,
        probe: Arc<dyn ReadinessProbe>,
    ) -> Self {
        Self {
            config,
            sender,
            probe,
            inner: Arc::new(Mutex::new(Inner {
                next_cycle_id: 1,
                current_cycle: None,
                last_ready_at: None,
                last_ready_cycle_id: 0,
            })),
        }
    }

    pub async fn ensure_ready(&self, max_wait: Duration) -> EnsureReadyResult {
        if let Some(cycle_id) = self.check_ready_cache().await {
            return EnsureReadyResult::Ready { cycle_id };
        }

        let cycle = self.get_or_start_cycle().await;
        self.wait_for_cycle(cycle, max_wait).await
    }

    async fn check_ready_cache(&self) -> Option<u64> {
        let inner = self.inner.lock().await;
        // A short ready cache avoids re-sending WoL packets during request bursts
        // right after the upstream has already become healthy.
        if let Some(last_ready_at) = inner.last_ready_at
            && last_ready_at.elapsed() <= self.config.ready_cache_ttl
        {
            return Some(inner.last_ready_cycle_id);
        }
        None
    }

    async fn get_or_start_cycle(&self) -> Arc<WakeCycle> {
        let mut inner = self.inner.lock().await;

        if let Some(current) = inner.current_cycle.clone()
            && current.result_tx.borrow().is_none()
        {
            return current;
        }

        let cycle_id = inner.next_cycle_id;
        inner.next_cycle_id += 1;

        let (state_tx, _) = watch::channel(WakeState::Sleeping);
        let (result_tx, _) = watch::channel(None);

        let cycle = Arc::new(WakeCycle {
            id: cycle_id,
            state_tx,
            result_tx,
        });

        inner.current_cycle = Some(cycle.clone());

        let this = self.clone();
        let cycle_clone = cycle.clone();
        tokio::spawn(async move {
            this.run_cycle(cycle_clone).await;
        });

        cycle
    }

    async fn wait_for_cycle(&self, cycle: Arc<WakeCycle>, max_wait: Duration) -> EnsureReadyResult {
        let mut rx = cycle.result_tx.subscribe();

        if let Some(outcome) = rx.borrow().clone() {
            return map_outcome(cycle.id, outcome);
        }

        let timeout = tokio::time::sleep(max_wait);
        tokio::pin!(timeout);

        tokio::select! {
            _ = rx.changed() => {
                if let Some(outcome) = rx.borrow().clone() {
                    map_outcome(cycle.id, outcome)
                } else {
                    EnsureReadyResult::StillWaking { cycle_id: cycle.id, state: *cycle.state_tx.borrow() }
                }
            }
            _ = &mut timeout => {
                EnsureReadyResult::StillWaking {
                    cycle_id: cycle.id,
                    state: *cycle.state_tx.borrow(),
                }
            }
        }
    }

    async fn run_cycle(&self, cycle: Arc<WakeCycle>) {
        let cycle_id = cycle.id;
        cycle.set_state(WakeState::Waking);
        info!(cycle_id, "starting wake cycle");

        if let Err(error) = self.sender.send_magic_packet().await {
            error!(cycle_id, ?error, "failed sending wol packet");
            cycle.set_state(WakeState::Failed);
            cycle.set_result(WakeOutcome::Failed(error.to_string()));
            self.finish_cycle(cycle_id, None).await;
            return;
        }

        let deadline = Instant::now() + self.config.wake_timeout;

        loop {
            if Instant::now() >= deadline {
                cycle.set_state(WakeState::TimedOut);
                cycle.set_result(WakeOutcome::TimedOut);
                info!(cycle_id, "wake cycle timed out");
                self.finish_cycle(cycle_id, None).await;
                return;
            }

            let ping_ok = self.probe.ping().await;
            if ping_ok {
                cycle.set_state(WakeState::PingReady);
            }

            let health_ok = if ping_ok {
                self.probe.health().await
            } else {
                false
            };
            if ping_ok && health_ok {
                cycle.set_state(WakeState::HttpReady);
                cycle.set_state(WakeState::Ready);
                cycle.set_result(WakeOutcome::Ready);
                info!(cycle_id, "upstream ready");
                self.finish_cycle(cycle_id, Some(Instant::now())).await;
                return;
            }

            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    async fn finish_cycle(&self, cycle_id: u64, ready_at: Option<Instant>) {
        let mut inner = self.inner.lock().await;
        if let Some(current) = inner.current_cycle.as_ref()
            && current.id == cycle_id
        {
            inner.current_cycle = None;
        }

        if let Some(ready_at) = ready_at {
            inner.last_ready_at = Some(ready_at);
            inner.last_ready_cycle_id = cycle_id;
        }
    }
}

fn map_outcome(cycle_id: u64, outcome: WakeOutcome) -> EnsureReadyResult {
    match outcome {
        WakeOutcome::Ready => EnsureReadyResult::Ready { cycle_id },
        WakeOutcome::TimedOut => EnsureReadyResult::TimedOut { cycle_id },
        WakeOutcome::Failed(reason) => EnsureReadyResult::Failed { cycle_id, reason },
    }
}

fn build_magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0_u8; 102];
    packet[0..6].fill(0xFF);

    for idx in 0..16 {
        let start = 6 + idx * 6;
        packet[start..start + 6].copy_from_slice(&mac);
    }

    packet
}

#[cfg(test)]
mod tests {
    use std::net::UdpSocket as StdUdpSocket;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use tokio::sync::watch;

    use crate::readiness::ReadinessProbe;

    use super::{
        EnsureReadyResult, UdpWakeSender, WakeConfig, WakeCoordinator, WakeCycle, WakeOutcome,
        WakePacketSender, WakeState,
    };

    #[derive(Clone)]
    struct MockWakeSender {
        sends: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl WakePacketSender for MockWakeSender {
        async fn send_magic_packet(&self) -> anyhow::Result<()> {
            self.sends.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct MockProbe {
        checks: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct NeverReadyProbe;

    #[async_trait]
    impl ReadinessProbe for NeverReadyProbe {
        async fn ping(&self) -> bool {
            true
        }

        async fn health(&self) -> bool {
            false
        }
    }

    #[derive(Clone)]
    struct FailingWakeSender;

    #[async_trait]
    impl WakePacketSender for FailingWakeSender {
        async fn send_magic_packet(&self) -> anyhow::Result<()> {
            anyhow::bail!("sender failure")
        }
    }

    #[async_trait]
    impl ReadinessProbe for MockProbe {
        async fn ping(&self) -> bool {
            true
        }

        async fn health(&self) -> bool {
            self.checks.fetch_add(1, Ordering::Relaxed) > 2
        }
    }

    #[tokio::test]
    async fn concurrent_requests_share_single_wake_cycle() {
        let sends = Arc::new(AtomicUsize::new(0));
        let sender = Arc::new(MockWakeSender {
            sends: sends.clone(),
        });
        let probe = Arc::new(MockProbe {
            checks: Arc::new(AtomicUsize::new(0)),
        });

        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(10),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
            },
            sender,
            probe,
        );

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let c = coordinator.clone();
            tasks.push(tokio::spawn(async move {
                c.ensure_ready(Duration::from_secs(1)).await
            }));
        }

        let mut ready_count = 0;
        for task in tasks {
            assert!(matches!(
                task.await.expect("task"),
                EnsureReadyResult::Ready { .. }
            ));
            ready_count += 1;
        }

        assert_eq!(ready_count, 8);
        assert_eq!(sends.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn ensure_ready_returns_still_waking_when_wait_budget_is_short() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(2),
                poll_interval: Duration::from_millis(50),
                ready_cache_ttl: Duration::from_secs(1),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
        );

        // The short max_wait forces the caller-visible "still waking" path
        // used for edge timeout protection.
        let result = coordinator.ensure_ready(Duration::from_millis(10)).await;
        assert!(matches!(result, EnsureReadyResult::StillWaking { .. }));
    }

    #[tokio::test]
    async fn ensure_ready_returns_timeout_when_deadline_expires() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_millis(80),
                poll_interval: Duration::from_millis(20),
                ready_cache_ttl: Duration::from_millis(1),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
        );

        let result = coordinator.ensure_ready(Duration::from_secs(1)).await;
        assert!(matches!(result, EnsureReadyResult::TimedOut { .. }));
    }

    #[tokio::test]
    async fn ensure_ready_returns_failed_when_sender_fails() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
            },
            Arc::new(FailingWakeSender),
            Arc::new(NeverReadyProbe),
        );

        let result = coordinator.ensure_ready(Duration::from_secs(1)).await;
        assert!(matches!(result, EnsureReadyResult::Failed { .. }));
    }

    #[tokio::test]
    async fn ready_cache_prevents_duplicate_wake_packets() {
        let sends = Arc::new(AtomicUsize::new(0));
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
            },
            Arc::new(MockWakeSender {
                sends: sends.clone(),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(10)),
            }),
        );

        let first = coordinator.ensure_ready(Duration::from_secs(1)).await;
        assert!(matches!(first, EnsureReadyResult::Ready { .. }));

        let second = coordinator.ensure_ready(Duration::from_millis(1)).await;
        assert!(matches!(second, EnsureReadyResult::Ready { .. }));
        assert_eq!(sends.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn udp_sender_emits_magic_packet_payload() {
        let receiver = StdUdpSocket::bind("127.0.0.1:0").expect("bind udp receiver");
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("timeout");
        let port = receiver.local_addr().expect("addr").port();

        let sender = UdpWakeSender::new(
            [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            "127.0.0.1".parse().expect("ip"),
            port,
        );
        sender.send_magic_packet().await.expect("send packet");

        let mut buf = [0_u8; 1024];
        let size = receiver.recv(&mut buf).expect("receive packet");
        assert_eq!(size, 102);
        assert_eq!(&buf[0..6], &[0xff; 6]);
    }

    #[tokio::test]
    async fn get_or_start_cycle_reuses_existing_inflight_cycle() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
        );

        let (state_tx, _) = watch::channel(WakeState::Waking);
        let (result_tx, _) = watch::channel(None);
        let cycle = Arc::new(WakeCycle {
            id: 99,
            state_tx,
            result_tx,
        });

        {
            let mut inner = coordinator.inner.lock().await;
            inner.current_cycle = Some(cycle.clone());
        }

        let reused = coordinator.get_or_start_cycle().await;
        assert_eq!(reused.id, 99);
    }

    #[tokio::test]
    async fn wait_for_cycle_returns_immediately_when_result_already_present() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
        );

        let (state_tx, _) = watch::channel(WakeState::Ready);
        let (result_tx, _) = watch::channel(Some(WakeOutcome::Ready));
        let cycle = Arc::new(WakeCycle {
            id: 7,
            state_tx,
            result_tx,
        });

        let result = coordinator
            .wait_for_cycle(cycle, Duration::from_secs(1))
            .await;
        assert!(matches!(result, EnsureReadyResult::Ready { cycle_id: 7 }));
    }

    #[tokio::test]
    async fn wait_for_cycle_handles_state_change_without_result() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
        );

        let (state_tx, _) = watch::channel(WakeState::Waking);
        let (result_tx, _) = watch::channel(None);
        let cycle = Arc::new(WakeCycle {
            id: 8,
            state_tx,
            result_tx,
        });

        let cycle_clone = cycle.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cycle_clone.state_tx.send_replace(WakeState::PingReady);
            cycle_clone.result_tx.send_replace(None);
        });

        let result = coordinator
            .wait_for_cycle(cycle, Duration::from_secs(1))
            .await;
        assert!(matches!(
            result,
            EnsureReadyResult::StillWaking {
                cycle_id: 8,
                state: WakeState::PingReady
            }
        ));
    }
}
