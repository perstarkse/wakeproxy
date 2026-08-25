use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, watch};
use tracing::{error, info, warn};

use crate::readiness::ReadinessProbe;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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
    /// Upstream is usable; `cycle_id` is `None` when readiness came from
    /// direct probing rather than a wake cycle.
    Ready {
        cycle_id: Option<u64>,
    },
    StillWaking {
        cycle_id: u64,
        state: WakeState,
    },
    TimedOut {
        cycle_id: u64,
    },
    Failed {
        cycle_id: u64,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
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
    keep_awake_gen: Arc<AtomicU64>,
    global_state_tx: watch::Sender<WakeStatusSnapshot>,
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
        let (global_state_tx, _) = watch::channel(WakeStatusSnapshot {
            cycle_id: None,
            state: WakeState::Sleeping,
            ready: false,
            reason: None,
        });

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
            keep_awake_gen: Arc::new(AtomicU64::new(0)),
            global_state_tx,
        }
    }

    pub async fn ensure_ready(&self, max_wait: Duration) -> EnsureReadyResult {
        if let Some(cycle_id) = self.check_ready_cache().await {
            return EnsureReadyResult::Ready { cycle_id };
        }

        let (cycle, _) = self.get_or_start_cycle().await;
        self.wait_for_cycle(cycle, max_wait).await
    }

    pub fn subscribe(&self) -> watch::Receiver<WakeStatusSnapshot> {
        self.global_state_tx.subscribe()
    }

    pub async fn start_wake(&self, keep_awake_for: Option<Duration>) -> WakeStartResult {
        let keep_awake_for =
            keep_awake_for.map(|duration| duration.min(self.config.keep_awake_max));

        if let Some(cycle_id) = self.check_ready_cache().await {
            self.start_keep_awake_if_extending(keep_awake_for).await;
            return WakeStartResult::AlreadyReady { cycle_id };
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
                cycle_id,
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

    /// Returns `Some(reported_cycle_id)` while the ready cache is fresh.
    /// The inner id is `None` when readiness was confirmed by probing the
    /// upstream directly instead of running a wake cycle, so callers must
    /// not treat it as a real cycle identifier.
    async fn check_ready_cache(&self) -> Option<Option<u64>> {
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

    async fn start_keep_awake_if_extending(&self, keep_awake_for: Option<Duration>) {
        let Some(duration) = keep_awake_for else {
            return;
        };

        let now = Instant::now();
        let requested_until = now + duration;
        let generation = {
            let mut inner = self.inner.lock().await;
            if inner
                .keep_awake_until
                .is_some_and(|until| until >= requested_until)
            {
                return;
            }
            inner.keep_awake_until = Some(requested_until);
            self.keep_awake_gen.fetch_add(1, Ordering::Relaxed) + 1
        };

        self.start_keep_awake(duration, generation);
    }

    fn start_keep_awake(&self, duration: Duration, generation: u64) {
        let keep_awake = self.keep_awake.clone();
        let inner = self.inner.clone();
        let keep_awake_gen = self.keep_awake_gen.clone();
        tokio::spawn(async move {
            if let Err(error) = keep_awake.keep_awake(duration).await {
                warn!(
                    ?error,
                    seconds = duration.as_secs(),
                    "keep-awake request failed"
                );

                let current_gen = keep_awake_gen.load(Ordering::Relaxed);
                if generation == current_gen {
                    let mut inner = inner.lock().await;
                    inner.keep_awake_until = None;
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
        self.publish_cycle_snapshot(cycle_id, WakeState::Waking, false, None);
        info!(cycle_id, "starting wake cycle");

        if let Err(error) = self.sender.send_magic_packet().await {
            error!(cycle_id, ?error, "failed sending wol packet");
            let outcome = WakeOutcome::Failed(error.to_string());
            cycle.set_state(WakeState::Failed);
            cycle.set_result(outcome.clone());
            self.publish_cycle_snapshot(
                cycle_id,
                WakeState::Failed,
                false,
                Some(error.to_string()),
            );
            self.finish_cycle(cycle_id, outcome, None).await;
            return;
        }

        let deadline = Instant::now() + self.config.wake_timeout;

        loop {
            if Instant::now() >= deadline {
                let outcome = WakeOutcome::TimedOut;
                cycle.set_state(WakeState::TimedOut);
                cycle.set_result(outcome.clone());
                self.publish_cycle_snapshot(cycle_id, WakeState::TimedOut, false, None);
                info!(cycle_id, "wake cycle timed out");
                self.finish_cycle(cycle_id, outcome, None).await;
                return;
            }

            let ping_ok = self.probe.ping().await;
            if ping_ok {
                cycle.set_state(WakeState::PingReady);
                self.publish_cycle_snapshot(cycle_id, WakeState::PingReady, false, None);
            }

            let health_ok = if ping_ok {
                self.probe.health().await
            } else {
                false
            };
            if ping_ok && health_ok {
                let outcome = WakeOutcome::Ready;
                cycle.set_state(WakeState::HttpReady);
                self.publish_cycle_snapshot(cycle_id, WakeState::HttpReady, false, None);
                cycle.set_state(WakeState::Ready);
                cycle.set_result(outcome.clone());
                self.publish_cycle_snapshot(cycle_id, WakeState::Ready, true, None);
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
        // Readiness here comes from probing, not from a wake cycle, so no
        // synthetic cycle id is invented; the last real cycle id (if any)
        // stays the one reported to clients.
        let reported_cycle_id = {
            let mut inner = self.inner.lock().await;
            inner.last_ready_at = Some(Instant::now());
            inner.last_outcome = None;
            inner.last_ready_cycle_id
        };
        self.publish_snapshot(WakeStatusSnapshot {
            cycle_id: reported_cycle_id,
            state: WakeState::Ready,
            ready: true,
            reason: None,
        });
    }

    fn publish_snapshot(&self, snapshot: WakeStatusSnapshot) {
        let _ = self.global_state_tx.send(snapshot);
    }

    fn publish_cycle_snapshot(
        &self,
        cycle_id: u64,
        state: WakeState,
        ready: bool,
        reason: Option<String>,
    ) {
        self.publish_snapshot(WakeStatusSnapshot {
            cycle_id: Some(cycle_id),
            state,
            ready,
            reason,
        });
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
        WakeOutcome::Ready => EnsureReadyResult::Ready {
            cycle_id: Some(cycle_id),
        },
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
mod tests;
