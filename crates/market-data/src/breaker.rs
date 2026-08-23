//! Per-provider circuit breaker for the kline failover chain.
//!
//! Motivation: when an upstream is hard-down (e.g. the Tencent kline
//! endpoint serving its `waf.tencent.com/501page.html` challenge), every
//! request used to pay the full failing attempt before falling through to
//! the next provider. The breaker remembers that state: while a provider's
//! circuit is Open the failover chain skips it entirely, and after the
//! cooldown a single HalfOpen probe decides whether it has recovered.
//!
//! Failure classification:
//! - [`DataError::WafBlocked`] opens the circuit immediately;
//! - [`DataError::Network`] / [`DataError::Timeout`] / [`DataError::RateLimited`]
//!   count toward a consecutive-failure threshold (transient blips tolerated);
//! - everything else (`Parse`, `Empty`, `NoProvider`, ...) is data-level and
//!   does not count while Closed.
//!
//! A failed HalfOpen probe re-opens the circuit with the cooldown doubled,
//! capped at [`BreakerConfig::max_cooldown`]; a successful probe closes it
//! and resets the cooldown to the base value.
//!
//! Timestamps use [`tokio::time::Instant`] so tests can drive the cooldowns
//! with `tokio::time::pause` / `advance` (tokio `test-util`).

use astock_core::DataError;
use dashmap::DashMap;
use parking_lot::Mutex;
use std::time::Duration;
use tokio::time::Instant;

/// Breaker tuning knobs.
#[derive(Debug, Clone, Copy)]
pub struct BreakerConfig {
    /// Base Open duration before the first HalfOpen probe (default 10 min).
    pub cooldown: Duration,
    /// Upper bound for the exponentially growing cooldown (default 1 h).
    pub max_cooldown: Duration,
    /// Consecutive Network/Timeout/RateLimited failures that open the
    /// circuit (default 3). WAF blocks always open immediately.
    pub failure_threshold: u32,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        BreakerConfig {
            cooldown: Duration::from_secs(600),
            max_cooldown: Duration::from_secs(3600),
            failure_threshold: 3,
        }
    }
}

/// Circuit state, exported for the data-source health panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// Requests flow normally.
    Closed,
    /// Provider is skipped until the cooldown elapses.
    Open,
    /// Cooldown elapsed; exactly one probe request is in flight.
    HalfOpen,
}

/// Snapshot of one provider's circuit, for `MarketData::provider_health`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProviderHealth {
    /// Provider name (`tencent`, `sina`, `eastmoney`, ...).
    pub name: String,
    /// Current circuit state.
    pub state: CircuitState,
    /// Seconds until the next probe is allowed; `Some` only while Open.
    pub cooldown_remaining_secs: Option<u64>,
    /// Whether the provider is configured (e.g. optional token-gated
    /// providers are registered but unavailable without their token/key).
    pub available: bool,
}

#[derive(Debug)]
struct Circuit {
    state: CircuitState,
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    /// Cooldown currently in force; doubles on each failed probe.
    cooldown: Duration,
    /// Configured-and-usable flag; unavailable providers never get traffic.
    available: bool,
}

impl Circuit {
    fn closed(cooldown: Duration) -> Self {
        Circuit {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            opened_at: None,
            cooldown,
            available: true,
        }
    }
}

/// How a failure affects a Closed circuit.
enum FailureKind {
    /// Opens the circuit immediately (WAF challenge).
    Trip,
    /// Counts toward `failure_threshold`.
    Count,
    /// Data-level error; does not affect the circuit.
    Ignore,
}

fn classify(err: &DataError) -> FailureKind {
    match err {
        DataError::WafBlocked(_) => FailureKind::Trip,
        DataError::Network { .. } | DataError::Timeout(_) | DataError::RateLimited(_) => {
            FailureKind::Count
        }
        _ => FailureKind::Ignore,
    }
}

/// Thread-safe registry of per-provider circuits. Cheap to share: wrap in
/// `Arc` or keep inside an `Arc`'d context (the hub does the latter).
pub struct CircuitBreaker {
    config: BreakerConfig,
    circuits: DashMap<String, Mutex<Circuit>>,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(BreakerConfig::default())
    }
}

impl CircuitBreaker {
    /// Build with custom tuning.
    pub fn new(config: BreakerConfig) -> Self {
        CircuitBreaker {
            config,
            circuits: DashMap::new(),
        }
    }

    /// Pre-create a Closed circuit so the health panel lists the provider
    /// before its first request.
    pub fn register(&self, name: &str) {
        self.circuits
            .entry(name.to_string())
            .or_insert_with(|| Mutex::new(Circuit::closed(self.config.cooldown)));
    }

    fn circuit(&self, name: &str) -> dashmap::mapref::one::RefMut<'_, String, Mutex<Circuit>> {
        self.register(name);
        self.circuits.get_mut(name).expect("registered above")
    }

    /// Mark a provider as (un)configured: unavailable providers are listed
    /// on the health panel but `allow_request` always refuses them. Used by
    /// the hub for optional token-gated providers (tushare, iwencai).
    pub fn set_available(&self, name: &str, available: bool) {
        let entry = self.circuit(name);
        entry.lock().available = available;
    }

    /// Whether a request to `name` may proceed right now.
    ///
    /// Open → HalfOpen transition happens here: the first caller after the
    /// cooldown becomes the probe; concurrent callers are told to skip until
    /// the probe reports back.
    pub fn allow_request(&self, name: &str) -> bool {
        let entry = self.circuit(name);
        let mut c = entry.lock();
        if !c.available {
            return false;
        }
        match c.state {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => false,
            CircuitState::Open => {
                let elapsed = c.opened_at.map(|t| t.elapsed()).unwrap_or(c.cooldown);
                if elapsed >= c.cooldown {
                    c.state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful (responsive) call: close the circuit, reset the
    /// failure count and the cooldown.
    pub fn on_success(&self, name: &str) {
        let entry = self.circuit(name);
        let mut c = entry.lock();
        c.state = CircuitState::Closed;
        c.consecutive_failures = 0;
        c.opened_at = None;
        c.cooldown = self.config.cooldown;
    }

    /// Open a circuit immediately for a best-effort supplementary source.
    /// This is used when a valid primary result already exists and allowing
    /// more callers to queue behind the same optional outage would only add
    /// latency without improving correctness.
    pub fn trip(&self, name: &str) {
        let entry = self.circuit(name);
        let mut c = entry.lock();
        c.state = CircuitState::Open;
        c.opened_at = Some(Instant::now());
        c.consecutive_failures = 0;
    }

    /// Record a failed call. See the module docs for the classification.
    pub fn on_failure(&self, name: &str, err: &DataError) {
        let entry = self.circuit(name);
        let mut c = entry.lock();
        if c.state == CircuitState::HalfOpen {
            // Probe failed: re-open with exponential backoff.
            c.cooldown = (c.cooldown * 2).min(self.config.max_cooldown);
            c.state = CircuitState::Open;
            c.opened_at = Some(Instant::now());
            c.consecutive_failures = 0;
            return;
        }
        match classify(err) {
            FailureKind::Trip => {
                c.state = CircuitState::Open;
                c.opened_at = Some(Instant::now());
                c.consecutive_failures = 0;
            }
            FailureKind::Count => {
                c.consecutive_failures += 1;
                if c.consecutive_failures >= self.config.failure_threshold {
                    c.state = CircuitState::Open;
                    c.opened_at = Some(Instant::now());
                    c.consecutive_failures = 0;
                }
            }
            FailureKind::Ignore => {}
        }
    }

    /// Current state of one provider's circuit (for tests/diagnostics).
    pub fn state(&self, name: &str) -> CircuitState {
        self.circuit(name).lock().state
    }

    /// Health snapshot of every registered provider, sorted by name.
    pub fn health(&self) -> Vec<ProviderHealth> {
        let mut out: Vec<ProviderHealth> = self
            .circuits
            .iter()
            .map(|entry| {
                let c = entry.value().lock();
                let cooldown_remaining_secs = if c.state == CircuitState::Open {
                    let elapsed = c.opened_at.map(|t| t.elapsed()).unwrap_or(c.cooldown);
                    Some(c.cooldown.saturating_sub(elapsed).as_secs())
                } else {
                    None
                };
                ProviderHealth {
                    name: entry.key().clone(),
                    state: c.state,
                    cooldown_remaining_secs,
                    available: c.available,
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waf() -> DataError {
        DataError::WafBlocked("tencent kline sh600519".to_string())
    }

    fn network() -> DataError {
        DataError::Network {
            host: "example.com".to_string(),
            message: "connection reset".to_string(),
        }
    }

    #[test]
    fn waf_opens_immediately_and_skips_while_open() {
        let cb = CircuitBreaker::default();
        assert!(cb.allow_request("tencent"));
        cb.on_failure("tencent", &waf());
        assert_eq!(cb.state("tencent"), CircuitState::Open);
        assert!(!cb.allow_request("tencent"), "open circuit must be skipped");
        let health = cb.health();
        let t = health.iter().find(|h| h.name == "tencent").unwrap();
        assert_eq!(t.state, CircuitState::Open);
        let remaining = t.cooldown_remaining_secs.unwrap();
        assert!((590..=600).contains(&remaining), "remaining={remaining}");
    }

    #[test]
    fn network_failures_open_at_threshold() {
        let cb = CircuitBreaker::default(); // threshold = 3
        cb.on_failure("sina", &network());
        cb.on_failure("sina", &DataError::Timeout("example.com".to_string()));
        assert_eq!(cb.state("sina"), CircuitState::Closed);
        assert!(cb.allow_request("sina"));
        cb.on_failure("sina", &network());
        assert_eq!(cb.state("sina"), CircuitState::Open);
        assert!(!cb.allow_request("sina"));
    }

    #[test]
    fn supplementary_source_can_be_opened_immediately() {
        let cb = CircuitBreaker::default();
        cb.trip("eastmoney_enrichment");
        assert_eq!(cb.state("eastmoney_enrichment"), CircuitState::Open);
        assert!(!cb.allow_request("eastmoney_enrichment"));
    }

    #[test]
    fn data_level_errors_do_not_trip() {
        let cb = CircuitBreaker::default();
        for _ in 0..10 {
            cb.on_failure("eastmoney", &DataError::Empty("no rows".to_string()));
            cb.on_failure(
                "eastmoney",
                &DataError::Parse {
                    upstream: "em".to_string(),
                    message: "bad json".to_string(),
                },
            );
        }
        assert_eq!(cb.state("eastmoney"), CircuitState::Closed);
        assert!(cb.allow_request("eastmoney"));
    }

    #[tokio::test(start_paused = true)]
    async fn half_open_probe_closes_on_success() {
        let cb = CircuitBreaker::default();
        cb.on_failure("tencent", &waf());
        assert!(!cb.allow_request("tencent"));

        tokio::time::advance(Duration::from_secs(599)).await;
        assert!(!cb.allow_request("tencent"), "cooldown not over yet");

        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(cb.allow_request("tencent"), "first caller probes");
        assert_eq!(cb.state("tencent"), CircuitState::HalfOpen);
        assert!(
            !cb.allow_request("tencent"),
            "only one probe in flight; others skip"
        );

        cb.on_success("tencent");
        assert_eq!(cb.state("tencent"), CircuitState::Closed);
        assert!(cb.allow_request("tencent"));
        let t = cb
            .health()
            .into_iter()
            .find(|h| h.name == "tencent")
            .unwrap();
        assert_eq!(t.cooldown_remaining_secs, None);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_probe_reopens_with_backoff_doubling() {
        let cb = CircuitBreaker::default(); // base 10 min, cap 1 h
        cb.on_failure("tencent", &waf());

        // Probe 1 fails → cooldown 20 min.
        tokio::time::advance(Duration::from_secs(600)).await;
        assert!(cb.allow_request("tencent"));
        cb.on_failure("tencent", &DataError::Timeout("gtimg".to_string()));
        assert_eq!(cb.state("tencent"), CircuitState::Open);

        tokio::time::advance(Duration::from_secs(600)).await;
        assert!(!cb.allow_request("tencent"), "10 min no longer enough");
        tokio::time::advance(Duration::from_secs(600)).await;
        assert!(cb.allow_request("tencent"), "20 min cooldown elapsed");

        // Probe 2 fails → cooldown 40 min.
        cb.on_failure("tencent", &waf());
        tokio::time::advance(Duration::from_secs(1200)).await;
        assert!(!cb.allow_request("tencent"));
        tokio::time::advance(Duration::from_secs(1200)).await;
        assert!(cb.allow_request("tencent"));

        // Probe 3 fails → 80 min would exceed the 60 min cap → 60 min.
        cb.on_failure("tencent", &waf());
        let t = cb
            .health()
            .into_iter()
            .find(|h| h.name == "tencent")
            .unwrap();
        assert_eq!(t.cooldown_remaining_secs, Some(3600));

        // Successful probe resets the cooldown to the base value.
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert!(cb.allow_request("tencent"));
        cb.on_success("tencent");
        cb.on_failure("tencent", &waf());
        let t = cb
            .health()
            .into_iter()
            .find(|h| h.name == "tencent")
            .unwrap();
        assert_eq!(t.cooldown_remaining_secs, Some(600));
    }

    #[test]
    fn unavailable_provider_is_listed_but_never_allows() {
        let cb = CircuitBreaker::default();
        cb.register("tushare");
        cb.set_available("tushare", false);
        assert!(!cb.allow_request("tushare"));
        let h = cb
            .health()
            .into_iter()
            .find(|h| h.name == "tushare")
            .unwrap();
        assert!(!h.available);
        assert_eq!(h.state, CircuitState::Closed);
        cb.set_available("tushare", true);
        assert!(cb.allow_request("tushare"));
    }

    #[test]
    fn success_resets_consecutive_failure_count() {
        let cb = CircuitBreaker::default();
        cb.on_failure("sina", &network());
        cb.on_failure("sina", &network());
        cb.on_success("sina");
        cb.on_failure("sina", &network());
        cb.on_failure("sina", &network());
        assert_eq!(cb.state("sina"), CircuitState::Closed);
    }
}
