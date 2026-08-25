use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

#[derive(Clone)]
pub(crate) struct WakeRateLimiter {
    inner: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
    max_starts: usize,
    window: Duration,
}

impl WakeRateLimiter {
    pub(crate) fn new(max_starts: usize, window: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_starts,
            window,
        }
    }

    pub(crate) async fn check_and_register(&self, ip: IpAddr) -> bool {
        let mut map = self.inner.lock().await;
        let now = Instant::now();

        // Prune expired entries on every call so memory stays bounded by IPs
        // seen inside the window (mirrors the login-attempt tracker).
        map.retain(|_, timestamps| {
            timestamps.retain(|ts| now.duration_since(*ts) <= self.window);
            !timestamps.is_empty()
        });

        let timestamps = map.entry(ip).or_default();
        timestamps.push(now);

        if timestamps.len() > self.max_starts {
            timestamps.pop();
            return false;
        }

        true
    }
}
