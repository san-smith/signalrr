//! Reconnect retry policies for [`super::HubConnection`].

use std::time::Duration;

/// Context passed to [`RetryPolicy::next_retry_delay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryContext {
    pub elapsed: Duration,
    pub previous_retry_count: usize,
    pub retry_reason: String,
}

/// Determines delays between automatic reconnect attempts.
pub trait RetryPolicy: Send + Sync {
    fn next_retry_delay(&self, context: &RetryContext) -> Option<Duration>;
}

/// Default retry delays: 0s, 2s, 10s, 30s, then stop.
pub const DEFAULT_RETRY_DELAYS: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_secs(2),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// SignalR default reconnect policy.
#[derive(Debug, Clone)]
pub struct DefaultReconnectPolicy {
    delays: Vec<Option<Duration>>,
}

impl Default for DefaultReconnectPolicy {
    fn default() -> Self {
        Self::new(DEFAULT_RETRY_DELAYS.iter().copied())
    }
}

impl DefaultReconnectPolicy {
    pub fn new(delays: impl IntoIterator<Item = Duration>) -> Self {
        Self {
            delays: delays.into_iter().map(Some).chain([None]).collect(),
        }
    }
}

impl RetryPolicy for DefaultReconnectPolicy {
    fn next_retry_delay(&self, context: &RetryContext) -> Option<Duration> {
        self.delays
            .get(context.previous_retry_count)
            .copied()
            .unwrap_or(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_returns_expected_delays() {
        let policy = DefaultReconnectPolicy::default();
        assert_eq!(
            policy.next_retry_delay(&RetryContext {
                elapsed: Duration::ZERO,
                previous_retry_count: 0,
                retry_reason: String::new(),
            }),
            Some(Duration::ZERO)
        );
        assert_eq!(
            policy.next_retry_delay(&RetryContext {
                elapsed: Duration::ZERO,
                previous_retry_count: 1,
                retry_reason: String::new(),
            }),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            policy.next_retry_delay(&RetryContext {
                elapsed: Duration::ZERO,
                previous_retry_count: 4,
                retry_reason: String::new(),
            }),
            None
        );
    }
}
