use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, FromRef, FromRequestParts};
use axum::http::HeaderValue;
use axum::http::request::Parts;

use crate::error::AppError;

/// Entries whose window has ended are dropped once the map grows past this, so memory stays
/// bounded without a sweeper task.
const PRUNE_AT: usize = 10_000;

/// Counts failures per key inside a fixed window. `max == 0` disables it.
// ponytail: global mutex + fixed window per process; shard or move to Redis when multi-instance
pub struct RateLimiter<K> {
    max: u32,
    window: Duration,
    hits: Mutex<HashMap<K, (u32, Instant)>>, // (failures, window start)
}

impl<K: Hash + Eq> RateLimiter<K> {
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// `Some(retry_after)` while the key is over the limit inside a live window.
    pub fn check(&self, key: &K) -> Option<Duration> {
        if self.max == 0 {
            return None;
        }
        let hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        let (count, start) = hits.get(key)?;
        let remaining = self.window.checked_sub(start.elapsed())?;
        (*count >= self.max).then_some(remaining)
    }

    pub fn record(&self, key: K) {
        if self.max == 0 {
            return;
        }
        let mut hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        if hits.len() > PRUNE_AT {
            Self::prune_locked(&mut hits, self.window);
        }
        let now = Instant::now();
        let entry = hits.entry(key).or_insert((0, now));
        if now.duration_since(entry.1) >= self.window {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    /// Drops entries whose window has ended.
    fn prune_locked(hits: &mut HashMap<K, (u32, Instant)>, window: Duration) {
        hits.retain(|_, (_, start)| start.elapsed() < window);
    }
}

/// Whether the service sits behind a proxy that appends `X-Forwarded-For`. Services expose it
/// from their state via `FromRef` so `ClientIp` can read it.
#[derive(Clone, Copy)]
pub struct BehindProxy(pub bool);

/// The caller's IP: the TCP peer, or behind a trusted proxy the rightmost `X-Forwarded-For` entry.
pub struct ClientIp(pub IpAddr);

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
    BehindProxy: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, AppError> {
        let ConnectInfo(peer) = ConnectInfo::<SocketAddr>::from_request_parts(parts, state).await?;
        let BehindProxy(behind_proxy) = BehindProxy::from_ref(state);
        Ok(Self(forwarded_ip(
            parts.headers.get("x-forwarded-for"),
            peer.ip(),
            behind_proxy,
        )))
    }
}

/// Only the rightmost entry is trusted: it is the one the proxy itself appended, while
/// everything before it arrived from the client and can be forged.
pub fn forwarded_ip(header: Option<&HeaderValue>, peer: IpAddr, behind_proxy: bool) -> IpAddr {
    if !behind_proxy {
        return peer;
    }
    header
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.rsplit(',').next())
        .and_then(|ip| ip.trim().parse().ok())
        .unwrap_or(peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn fixed_window_blocks_then_resets() {
        let limiter = RateLimiter::new(2, Duration::from_millis(50));
        let key = "a@b.c".to_string();
        assert!(limiter.check(&key).is_none());
        limiter.record(key.clone());
        assert!(limiter.check(&key).is_none());
        limiter.record(key.clone());
        assert!(limiter.check(&key).is_some());

        std::thread::sleep(Duration::from_millis(60));
        assert!(limiter.check(&key).is_none());
        limiter.record(key.clone());
        assert!(limiter.check(&key).is_none(), "window restarted at 1");
    }

    #[test]
    fn record_prunes_when_large() {
        let limiter = RateLimiter::new(5, Duration::from_millis(1));
        for i in 0..=PRUNE_AT {
            limiter.record(i);
        }
        std::thread::sleep(Duration::from_millis(5));
        limiter.record(usize::MAX);
        let hits = limiter.hits.lock().unwrap();
        assert_eq!(hits.len(), 1, "expired entries were dropped on record");
        assert!(hits.contains_key(&usize::MAX));
    }

    #[test]
    fn zero_disables() {
        let limiter = RateLimiter::new(0, Duration::from_secs(60));
        for _ in 0..10 {
            limiter.record(1u8);
        }
        assert!(limiter.check(&1u8).is_none());
    }

    #[test]
    fn forwarded_for_only_behind_proxy() {
        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let header = HeaderValue::from_static("1.1.1.1, 10.0.0.9");
        assert_eq!(forwarded_ip(Some(&header), peer, false), peer);
        assert_eq!(
            forwarded_ip(Some(&header), peer, true),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9))
        );
        assert_eq!(forwarded_ip(None, peer, true), peer);
        let garbage = HeaderValue::from_static("1.1.1.1, not-an-ip");
        assert_eq!(forwarded_ip(Some(&garbage), peer, true), peer);
    }
}
