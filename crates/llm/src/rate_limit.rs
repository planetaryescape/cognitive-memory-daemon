//! Token-bucket rate limiter shared across all LLM clients.
//!
//! The daemon's central efficiency win for extraction (and for LLM calls
//! generally) is that *all* agents go through one rate-limited gate.
//! Per-provider buckets avoid one provider's 429s starving the others.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[async_trait::async_trait]
pub trait RateLimiter: Send + Sync {
    /// Block until the caller may make one request, or return an error if
    /// the limiter is disabled and rate-limit handling is up to the caller.
    async fn acquire(&self);
}

/// Simple token bucket. `capacity` tokens, refilled at `refill_per_second`.
/// Backpressure: callers `await acquire()` and the bucket may sleep them.
pub struct TokenBucket {
    capacity: f64,
    refill_per_second: f64,
    inner: Mutex<TokenBucketState>,
}

struct TokenBucketState {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(capacity: f64, refill_per_second: f64) -> Self {
        Self {
            capacity,
            refill_per_second,
            inner: Mutex::new(TokenBucketState {
                tokens: capacity,
                last_refill: Instant::now(),
            }),
        }
    }

    fn try_consume_one(&self) -> Option<Duration> {
        let mut state = self.inner.lock().ok()?;
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill).as_secs_f64();
        state.tokens = (state.tokens + elapsed * self.refill_per_second).min(self.capacity);
        state.last_refill = now;
        if state.tokens >= 1.0 {
            state.tokens -= 1.0;
            None
        } else {
            // Wait until one token is available.
            let needed = 1.0 - state.tokens;
            Some(Duration::from_secs_f64(needed / self.refill_per_second))
        }
    }
}

#[async_trait::async_trait]
impl RateLimiter for TokenBucket {
    async fn acquire(&self) {
        loop {
            match self.try_consume_one() {
                None => return,
                Some(wait) => tokio::time::sleep(wait).await,
            }
        }
    }
}
