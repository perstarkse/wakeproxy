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
    assert!(matches!(
        result,
        EnsureReadyResult::Ready { cycle_id: Some(7) }
    ));
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
#[tokio::test]
async fn status_snapshot_reports_direct_ready_without_synthetic_cycle_id() {
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

    let snapshot = coordinator.status_snapshot().await;
    assert_eq!(snapshot.state, WakeState::Ready);
    assert!(snapshot.ready);
    assert_eq!(snapshot.cycle_id, None);
    assert_eq!(sends.load(Ordering::Relaxed), 0);

    // The fresh ready cache must keep reporting no cycle id and must not
    // trigger a redundant wake packet.
    let snapshot = coordinator.status_snapshot().await;
    assert!(snapshot.ready);
    assert_eq!(snapshot.cycle_id, None);
    assert_eq!(sends.load(Ordering::Relaxed), 0);
}
