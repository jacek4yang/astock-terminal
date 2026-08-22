//! Adaptive pacing: a tiny token bucket (burst 3, refill 1 per 2 s by
//! default) so the provider never hammers iwencai.

use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;

/// Token-bucket rate limiter shared by all requests of one client.
pub struct Pacer {
    state: Mutex<State>,
    interval: Duration,
    capacity: f64,
}

struct State {
    tokens: f64,
    last_refill: Instant,
}

impl Pacer {
    /// `interval` = time to earn one token; `burst` = bucket capacity.
    pub fn new(interval: Duration, burst: u32) -> Self {
        Self {
            state: Mutex::new(State {
                tokens: f64::from(burst),
                last_refill: Instant::now(),
            }),
            interval,
            capacity: f64::from(burst),
        }
    }

    /// Wait until one request token is available, then consume it.
    pub async fn wait(&self) {
        loop {
            let sleep_for = {
                let mut st = self.state.lock().expect("pacer poisoned");
                let now = Instant::now();
                let earned =
                    now.duration_since(st.last_refill).as_secs_f64() / self.interval.as_secs_f64();
                if earned > 0.0 {
                    st.tokens = (st.tokens + earned).min(self.capacity);
                    st.last_refill = now;
                }
                if st.tokens >= 1.0 {
                    st.tokens -= 1.0;
                    return;
                }
                // Time until the next full token.
                Duration::from_secs_f64(self.interval.as_secs_f64() * (1.0 - st.tokens))
            };
            tokio::time::sleep(sleep_for).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn burst_then_throttle() {
        let pacer = Pacer::new(Duration::from_secs(2), 3);
        let t0 = Instant::now();
        pacer.wait().await;
        pacer.wait().await;
        pacer.wait().await;
        assert_eq!(t0.elapsed(), Duration::ZERO, "burst of 3 should be instant");
        pacer.wait().await;
        assert_eq!(
            t0.elapsed(),
            Duration::from_secs(2),
            "4th request waits one interval"
        );
        pacer.wait().await;
        assert_eq!(t0.elapsed(), Duration::from_secs(4));
    }
}
