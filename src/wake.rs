use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, watch};
use tracing::{error, info, warn};

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
pub struct WakeStatusSnapshot {
    pub cycle_id: Option<u64>,
    pub state: WakeState,
    pub ready: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeStartResult {
    Started { cycle_id: u64 },
    AlreadyWaking { cycle_id: u64 },
    AlreadyReady { cycle_id: Option<u64> },
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
    pub keep_awake_max: Duration,
}

#[derive(Debug, Error)]
pub enum KeepAwakeError {
    #[error("keep-awake command failed")]
    Io(#[from] std::io::Error),
    #[error("keep-awake command exited with status {status}: {stderr}")]
    CommandFailed { status: i32, stderr: String },
}

#[derive(Debug, Error)]
pub enum WakeSendError {
    #[error("failed to send wake packet")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
}

#[async_trait]
pub trait WakePacketSender: Send + Sync {
    async fn send_magic_packet(&self) -> Result<(), WakeSendError>;
}

#[async_trait]
pub trait KeepAwakeRunner: Send + Sync {
    async fn keep_awake(&self, duration: Duration) -> Result<(), KeepAwakeError>;
}

#[derive(Clone)]
pub struct NoopKeepAwakeRunner;

#[async_trait]
impl KeepAwakeRunner for NoopKeepAwakeRunner {
    async fn keep_awake(&self, _duration: Duration) -> Result<(), KeepAwakeError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct CommandKeepAwakeRunner {
    command: PathBuf,
}

impl CommandKeepAwakeRunner {
    pub fn new(command: impl AsRef<Path>) -> Self {
        Self {
            command: command.as_ref().to_path_buf(),
        }
    }
}

#[async_trait]
impl KeepAwakeRunner for CommandKeepAwakeRunner {
    async fn keep_awake(&self, duration: Duration) -> Result<(), KeepAwakeError> {
        let output = tokio::process::Command::new(&self.command)
            .arg(duration.as_secs().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(KeepAwakeError::CommandFailed {
            status: output.status.code().unwrap_or_default(),
            stderr,
        })
    }
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
    async fn send_magic_packet(&self) -> Result<(), WakeSendError> {
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
    keep_awake: Arc<dyn KeepAwakeRunner>,
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    next_cycle_id: u64,
    current_cycle: Option<Arc<WakeCycle>>,
    last_ready_at: Option<Instant>,
    last_ready_cycle_id: Option<u64>,
    last_outcome: Option<WakeOutcome>,
    pending_keep_awake: Option<Duration>,
    keep_awake_until: Option<Instant>,
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
        keep_awake: Arc<dyn KeepAwakeRunner>,
    ) -> Self {
        Self {
            config,
            sender,
            probe,
            keep_awake,
            inner: Arc::new(Mutex::new(Inner {
                next_cycle_id: 1,
                current_cycle: None,
                last_ready_at: None,
                last_ready_cycle_id: None,
                last_outcome: None,
                pending_keep_awake: None,
                keep_awake_until: None,
            })),
        }
    }

    pub async fn ensure_ready(&self, max_wait: Duration) -> EnsureReadyResult {
        if let Some(cycle_id) = self.check_ready_cache().await {
            return EnsureReadyResult::Ready { cycle_id };
        }

        let (cycle, _) = self.get_or_start_cycle().await;
        self.wait_for_cycle(cycle, max_wait).await
    }

    pub async fn start_wake(&self, keep_awake_for: Option<Duration>) -> WakeStartResult {
        let keep_awake_for =
            keep_awake_for.map(|duration| duration.min(self.config.keep_awake_max));

        if let Some(cycle_id) = self.check_ready_cache().await {
            self.start_keep_awake_if_extending(keep_awake_for).await;
            return WakeStartResult::AlreadyReady {
                cycle_id: Some(cycle_id),
            };
        }

        {
            let mut inner = self.inner.lock().await;
            inner.pending_keep_awake = max_duration(inner.pending_keep_awake, keep_awake_for);
        }

        let (cycle, started_new) = self.get_or_start_cycle().await;
        if started_new {
            WakeStartResult::Started { cycle_id: cycle.id }
        } else {
            WakeStartResult::AlreadyWaking { cycle_id: cycle.id }
        }
    }

    pub async fn status_snapshot(&self) -> WakeStatusSnapshot {
        if let Some(cycle_id) = self.check_ready_cache().await {
            return WakeStatusSnapshot {
                cycle_id: Some(cycle_id),
                state: WakeState::Ready,
                ready: true,
                reason: None,
            };
        }

        let (current_cycle, last_outcome, last_ready_cycle_id) = {
            let inner = self.inner.lock().await;
            (
                inner.current_cycle.clone(),
                inner.last_outcome.clone(),
                inner.last_ready_cycle_id,
            )
        };

        if let Some(cycle) = current_cycle {
            if let Some(outcome) = cycle.result_tx.borrow().clone() {
                return map_outcome_snapshot(Some(cycle.id), outcome);
            }

            return WakeStatusSnapshot {
                cycle_id: Some(cycle.id),
                state: *cycle.state_tx.borrow(),
                ready: false,
                reason: None,
            };
        }

        let ping_ok = self.probe.ping().await;
        if ping_ok && self.probe.health().await {
            self.mark_direct_ready().await;
            return WakeStatusSnapshot {
                cycle_id: last_ready_cycle_id,
                state: WakeState::Ready,
                ready: true,
                reason: None,
            };
        }

        if let Some(outcome) = last_outcome {
            return map_outcome_snapshot(None, outcome);
        }

        WakeStatusSnapshot {
            cycle_id: None,
            state: WakeState::Sleeping,
            ready: false,
            reason: None,
        }
    }

    async fn check_ready_cache(&self) -> Option<u64> {
        let inner = self.inner.lock().await;
        // A short ready cache avoids re-sending WoL packets during request bursts
        // right after the upstream has already become healthy.
        if let Some(last_ready_at) = inner.last_ready_at
            && last_ready_at.elapsed() <= self.config.ready_cache_ttl
        {
            return inner.last_ready_cycle_id;
        }
        None
    }

    async fn start_keep_awake_if_extending(&self, keep_awake_for: Option<Duration>) {
        let Some(duration) = keep_awake_for else {
            return;
        };

        let now = Instant::now();
        let requested_until = now + duration;
        let previous_until = {
            let mut inner = self.inner.lock().await;
            if inner
                .keep_awake_until
                .is_some_and(|until| until >= requested_until)
            {
                return;
            }
            let previous_until = inner.keep_awake_until;
            inner.keep_awake_until = Some(requested_until);
            previous_until
        };

        self.start_keep_awake(duration, requested_until, previous_until);
    }

    fn start_keep_awake(
        &self,
        duration: Duration,
        requested_until: Instant,
        previous_until: Option<Instant>,
    ) {
        let keep_awake = self.keep_awake.clone();
        let inner = self.inner.clone();
        tokio::spawn(async move {
            if let Err(error) = keep_awake.keep_awake(duration).await {
                warn!(
                    ?error,
                    seconds = duration.as_secs(),
                    "keep-awake request failed"
                );

                let mut inner = inner.lock().await;
                if inner.keep_awake_until == Some(requested_until) {
                    inner.keep_awake_until = previous_until;
                }
            }
        });
    }

    async fn get_or_start_cycle(&self) -> (Arc<WakeCycle>, bool) {
        let mut inner = self.inner.lock().await;

        if let Some(current) = inner.current_cycle.clone()
            && current.result_tx.borrow().is_none()
        {
            return (current, false);
        }

        let cycle = self.start_cycle_locked(&mut inner);

        let this = self.clone();
        let cycle_clone = cycle.clone();
        tokio::spawn(async move {
            this.run_cycle(cycle_clone).await;
        });

        (cycle, true)
    }

    fn start_cycle_locked(&self, inner: &mut Inner) -> Arc<WakeCycle> {
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
        inner.last_outcome = None;
        inner.keep_awake_until = None;
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
            let outcome = WakeOutcome::Failed(error.to_string());
            cycle.set_state(WakeState::Failed);
            cycle.set_result(outcome.clone());
            self.finish_cycle(cycle_id, outcome, None).await;
            return;
        }

        let deadline = Instant::now() + self.config.wake_timeout;

        loop {
            if Instant::now() >= deadline {
                let outcome = WakeOutcome::TimedOut;
                cycle.set_state(WakeState::TimedOut);
                cycle.set_result(outcome.clone());
                info!(cycle_id, "wake cycle timed out");
                self.finish_cycle(cycle_id, outcome, None).await;
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
                let outcome = WakeOutcome::Ready;
                cycle.set_state(WakeState::HttpReady);
                cycle.set_state(WakeState::Ready);
                cycle.set_result(outcome.clone());
                info!(cycle_id, "upstream ready");
                self.finish_cycle(cycle_id, outcome, Some(Instant::now()))
                    .await;
                self.take_and_start_pending_keep_awake().await;
                return;
            }

            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    async fn finish_cycle(&self, cycle_id: u64, outcome: WakeOutcome, ready_at: Option<Instant>) {
        let mut inner = self.inner.lock().await;
        if let Some(current) = inner.current_cycle.as_ref()
            && current.id == cycle_id
        {
            inner.current_cycle = None;
        }

        if let Some(ready_at) = ready_at {
            inner.last_ready_at = Some(ready_at);
            inner.last_ready_cycle_id = Some(cycle_id);
            inner.last_outcome = None;
            return;
        }

        inner.pending_keep_awake = None;
        if !matches!(outcome, WakeOutcome::Ready) {
            inner.last_outcome = Some(outcome);
        }
    }

    async fn take_and_start_pending_keep_awake(&self) {
        let pending_keep_awake = {
            let mut inner = self.inner.lock().await;
            inner.pending_keep_awake.take()
        };
        self.start_keep_awake_if_extending(pending_keep_awake).await;
    }

    async fn mark_direct_ready(&self) {
        let mut inner = self.inner.lock().await;
        inner.last_ready_at = Some(Instant::now());
        inner.last_outcome = None;
    }
}

fn max_duration(left: Option<Duration>, right: Option<Duration>) -> Option<Duration> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(duration), None) | (None, Some(duration)) => Some(duration),
        (None, None) => None,
    }
}

fn map_outcome(cycle_id: u64, outcome: WakeOutcome) -> EnsureReadyResult {
    match outcome {
        WakeOutcome::Ready => EnsureReadyResult::Ready { cycle_id },
        WakeOutcome::TimedOut => EnsureReadyResult::TimedOut { cycle_id },
        WakeOutcome::Failed(reason) => EnsureReadyResult::Failed { cycle_id, reason },
    }
}

fn map_outcome_snapshot(cycle_id: Option<u64>, outcome: WakeOutcome) -> WakeStatusSnapshot {
    match outcome {
        WakeOutcome::Ready => WakeStatusSnapshot {
            cycle_id,
            state: WakeState::Ready,
            ready: true,
            reason: None,
        },
        WakeOutcome::TimedOut => WakeStatusSnapshot {
            cycle_id,
            state: WakeState::TimedOut,
            ready: false,
            reason: None,
        },
        WakeOutcome::Failed(reason) => WakeStatusSnapshot {
            cycle_id,
            state: WakeState::Failed,
            ready: false,
            reason: Some(reason),
        },
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
    use tokio::sync::{Mutex, watch};

    use crate::readiness::ReadinessProbe;

    use super::{
        EnsureReadyResult, KeepAwakeError, KeepAwakeRunner, NoopKeepAwakeRunner, UdpWakeSender,
        WakeConfig, WakeCoordinator, WakeCycle, WakeOutcome, WakePacketSender, WakeSendError,
        WakeStartResult, WakeState,
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
    struct MockProbe {
        checks: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct MockKeepAwakeRunner {
        durations: Arc<Mutex<Vec<Duration>>>,
        fail: bool,
    }

    #[async_trait]
    impl KeepAwakeRunner for MockKeepAwakeRunner {
        async fn keep_awake(&self, duration: Duration) -> Result<(), KeepAwakeError> {
            self.durations.lock().await.push(duration);
            if self.fail {
                return Err(KeepAwakeError::CommandFailed {
                    status: 1,
                    stderr: "simulated keep-awake failure".to_string(),
                });
            }
            Ok(())
        }
    }

    #[derive(Clone)]
    struct NeverReadyProbe;

    #[derive(Clone)]
    struct PingDownProbe;

    #[async_trait]
    impl ReadinessProbe for NeverReadyProbe {
        async fn ping(&self) -> bool {
            true
        }

        async fn health(&self) -> bool {
            false
        }
    }

    #[async_trait]
    impl ReadinessProbe for PingDownProbe {
        async fn ping(&self) -> bool {
            false
        }

        async fn health(&self) -> bool {
            false
        }
    }

    #[derive(Clone)]
    struct FailingWakeSender;

    #[async_trait]
    impl WakePacketSender for FailingWakeSender {
        async fn send_magic_packet(&self) -> Result<(), WakeSendError> {
            Err(WakeSendError::Message("sender failure".to_string()))
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
                keep_awake_max: Duration::from_secs(14_400),
            },
            sender,
            probe,
            Arc::new(NoopKeepAwakeRunner),
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
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
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
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
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
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(FailingWakeSender),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
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
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: sends.clone(),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(10)),
            }),
            Arc::new(NoopKeepAwakeRunner),
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
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
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

        let (reused, started_new) = coordinator.get_or_start_cycle().await;
        assert_eq!(reused.id, 99);
        assert!(!started_new);
    }

    #[tokio::test]
    async fn wait_for_cycle_returns_immediately_when_result_already_present() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
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
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
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

    #[tokio::test]
    async fn status_snapshot_reports_sleeping_when_probe_is_not_ready() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
        );

        let snapshot = coordinator.status_snapshot().await;
        assert_eq!(snapshot.state, WakeState::Sleeping);
        assert!(!snapshot.ready);
    }

    #[tokio::test]
    async fn start_wake_keeps_longest_pending_keep_awake_duration() {
        let durations = Arc::new(Mutex::new(Vec::new()));
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(MockKeepAwakeRunner {
                durations: durations.clone(),
                fail: false,
            }),
        );

        let _ = coordinator
            .start_wake(Some(Duration::from_secs(3_600)))
            .await;
        let _ = coordinator.start_wake(Some(Duration::from_secs(900))).await;
        tokio::time::sleep(Duration::from_millis(80)).await;

        let seen = durations.lock().await.clone();
        assert_eq!(seen, vec![Duration::from_secs(3_600)]);
    }

    #[tokio::test]
    async fn start_wake_ignores_shorter_ready_cache_keep_awake_request() {
        let durations = Arc::new(Mutex::new(Vec::new()));
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(10)),
            }),
            Arc::new(MockKeepAwakeRunner {
                durations: durations.clone(),
                fail: false,
            }),
        );

        let _ = coordinator
            .start_wake(Some(Duration::from_secs(3_600)))
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = coordinator.start_wake(Some(Duration::from_secs(900))).await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        let seen = durations.lock().await.clone();
        assert_eq!(seen, vec![Duration::from_secs(3_600)]);
    }

    #[tokio::test]
    async fn start_wake_extends_ready_cache_keep_awake_request() {
        let durations = Arc::new(Mutex::new(Vec::new()));
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(10)),
            }),
            Arc::new(MockKeepAwakeRunner {
                durations: durations.clone(),
                fail: false,
            }),
        );

        let _ = coordinator.start_wake(Some(Duration::from_secs(900))).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = coordinator
            .start_wake(Some(Duration::from_secs(3_600)))
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        let seen = durations.lock().await.clone();
        assert_eq!(
            seen,
            vec![Duration::from_secs(900), Duration::from_secs(3_600)]
        );
    }

    #[tokio::test]
    async fn start_wake_reports_existing_cycle_when_already_inflight() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
        );

        let first = coordinator.start_wake(None).await;
        let second = coordinator.start_wake(None).await;

        assert!(matches!(first, WakeStartResult::Started { .. }));
        assert!(matches!(second, WakeStartResult::AlreadyWaking { .. }));
    }

    #[tokio::test]
    async fn start_wake_does_not_request_keep_awake_before_ready() {
        let durations = Arc::new(Mutex::new(Vec::new()));
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(3_600),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(MockKeepAwakeRunner {
                durations: durations.clone(),
                fail: false,
            }),
        );

        let _ = coordinator.start_wake(Some(Duration::from_secs(600))).await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        let seen = durations.lock().await.clone();
        assert!(seen.is_empty());
    }

    #[tokio::test]
    async fn start_wake_reports_already_ready_from_recent_cache() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(10)),
            }),
            Arc::new(NoopKeepAwakeRunner),
        );

        assert!(matches!(
            coordinator.ensure_ready(Duration::from_secs(1)).await,
            EnsureReadyResult::Ready { .. }
        ));

        let start = coordinator.start_wake(None).await;
        assert!(matches!(
            start,
            WakeStartResult::AlreadyReady { cycle_id: Some(_) }
        ));
    }

    #[tokio::test]
    async fn status_snapshot_reports_ready_from_recent_cache() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(10)),
            }),
            Arc::new(NoopKeepAwakeRunner),
        );

        assert!(matches!(
            coordinator.ensure_ready(Duration::from_secs(1)).await,
            EnsureReadyResult::Ready { .. }
        ));

        let snapshot = coordinator.status_snapshot().await;
        assert_eq!(snapshot.state, WakeState::Ready);
        assert!(snapshot.ready);
        assert!(snapshot.cycle_id.is_some());
    }

    #[tokio::test]
    async fn status_snapshot_reports_current_cycle_state_without_result() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
        );

        let (state_tx, _) = watch::channel(WakeState::Waking);
        let (result_tx, _) = watch::channel(None);
        let cycle = Arc::new(WakeCycle {
            id: 44,
            state_tx,
            result_tx,
        });

        {
            let mut inner = coordinator.inner.lock().await;
            inner.current_cycle = Some(cycle);
        }

        let snapshot = coordinator.status_snapshot().await;
        assert_eq!(snapshot.state, WakeState::Waking);
        assert!(!snapshot.ready);
        assert_eq!(snapshot.cycle_id, Some(44));
    }

    #[tokio::test]
    async fn status_snapshot_reports_cycle_result_when_present() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
        );

        let (state_tx, _) = watch::channel(WakeState::Ready);
        let (result_tx, _) = watch::channel(Some(WakeOutcome::Ready));
        let cycle = Arc::new(WakeCycle {
            id: 45,
            state_tx,
            result_tx,
        });

        {
            let mut inner = coordinator.inner.lock().await;
            inner.current_cycle = Some(cycle);
        }

        let snapshot = coordinator.status_snapshot().await;
        assert_eq!(snapshot.state, WakeState::Ready);
        assert!(snapshot.ready);
        assert_eq!(snapshot.cycle_id, Some(45));
    }

    #[tokio::test]
    async fn status_snapshot_reports_last_timeout_outcome() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(NeverReadyProbe),
            Arc::new(NoopKeepAwakeRunner),
        );

        {
            let mut inner = coordinator.inner.lock().await;
            inner.last_outcome = Some(WakeOutcome::TimedOut);
        }

        let snapshot = coordinator.status_snapshot().await;
        assert_eq!(snapshot.state, WakeState::TimedOut);
        assert!(!snapshot.ready);
    }

    #[tokio::test]
    async fn ensure_ready_with_ping_down_probe_times_out() {
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_millis(80),
                poll_interval: Duration::from_millis(20),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(14_400),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(PingDownProbe),
            Arc::new(NoopKeepAwakeRunner),
        );

        let result = coordinator.ensure_ready(Duration::from_secs(1)).await;
        assert!(matches!(result, EnsureReadyResult::TimedOut { .. }));
    }

    #[tokio::test]
    async fn ping_down_probe_health_method_returns_false() {
        let probe = PingDownProbe;
        assert!(!probe.health().await);
    }

    #[tokio::test]
    async fn start_wake_requests_keep_awake_after_ready() {
        let durations = Arc::new(Mutex::new(Vec::new()));
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(3_600),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(10)),
            }),
            Arc::new(MockKeepAwakeRunner {
                durations: durations.clone(),
                fail: false,
            }),
        );

        let _ = coordinator.start_wake(Some(Duration::from_secs(600))).await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        let seen = durations.lock().await.clone();
        assert_eq!(seen, vec![Duration::from_secs(600)]);
    }

    #[tokio::test]
    async fn start_wake_bounds_keep_awake_duration() {
        let durations = Arc::new(Mutex::new(Vec::new()));
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(900),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(10)),
            }),
            Arc::new(MockKeepAwakeRunner {
                durations: durations.clone(),
                fail: false,
            }),
        );

        let _ = coordinator
            .start_wake(Some(Duration::from_secs(3_600)))
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        let seen = durations.lock().await.clone();
        assert_eq!(seen, vec![Duration::from_secs(900)]);
    }

    #[tokio::test]
    async fn keep_awake_failure_does_not_change_start_wake_result() {
        let durations = Arc::new(Mutex::new(Vec::new()));
        let coordinator = WakeCoordinator::new(
            WakeConfig {
                wake_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
                ready_cache_ttl: Duration::from_secs(1),
                keep_awake_max: Duration::from_secs(3_600),
            },
            Arc::new(MockWakeSender {
                sends: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(MockProbe {
                checks: Arc::new(AtomicUsize::new(10)),
            }),
            Arc::new(MockKeepAwakeRunner {
                durations: durations.clone(),
                fail: true,
            }),
        );

        let start = coordinator.start_wake(Some(Duration::from_secs(600))).await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert!(matches!(start, WakeStartResult::Started { .. }));
        let seen = durations.lock().await.clone();
        assert_eq!(seen, vec![Duration::from_secs(600)]);
    }
}
