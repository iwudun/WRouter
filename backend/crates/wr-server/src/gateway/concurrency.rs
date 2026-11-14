//! 每 provider 并发上限(镜像 Python `application/gateway/concurrency.py`
//! 与 `ConcurrencyConfig`,R2.12)。
//!
//! - `queue_timeout_ms == 0`:槽满即快速失败 429(不排队);
//! - `queue_timeout_ms > 0`:最多等待该毫秒数,超时 429;
//! - provider 级 `max_in_flight` 覆盖全局默认(默认 8);
//! - 流式路径带 abort 标志:客户端已断开时返回 499 且不打上游。
//!
//! 配置以「acquire 前同步快照」的方式生效(镜像 Python
//! `update_config`):`sync_config` 在每次转发前调用。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use wr_domain::provider::Provider;
use wr_infra::config::ConcurrencyConfig;

/// acquire 结果(镜像 `AcquireResult`)。
pub enum AcquireOutcome {
    Acquired(OwnedSemaphorePermit),
    Rejected {
        status: u16,
        message: String,
        aborted: bool,
    },
}

impl AcquireOutcome {
    pub fn ok(&self) -> bool {
        matches!(self, Self::Acquired(_))
    }
}

#[derive(Default)]
pub struct ConcurrencyLimiter {
    config: std::sync::Mutex<Option<ConcurrencyConfig>>,
    queues: std::sync::Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl ConcurrencyLimiter {
    /// 以快照中的配置同步限流参数(每请求调用,廉价)。
    pub fn sync_config(&self, config: &ConcurrencyConfig) {
        *self.config.lock().unwrap() = Some(config.clone());
    }

    fn current_config(&self) -> ConcurrencyConfig {
        self.config.lock().unwrap().clone().unwrap_or_default()
    }

    fn queue(&self, provider: &Provider, default_cap: i64) -> Arc<Semaphore> {
        let cap = match provider.max_in_flight {
            Some(value) if value > 0 => value,
            _ => default_cap,
        };
        let mut queues = self.queues.lock().unwrap();
        queues
            .entry(provider.id.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(cap.max(1) as usize)))
            .clone()
    }

    /// 申请一个 in-flight 槽位。`aborted` 由流式路径提供(客户端断开)。
    pub async fn acquire(&self, provider: &Provider, aborted: impl Fn() -> bool) -> AcquireOutcome {
        let config = self.current_config();
        if !config.enabled {
            // 禁用时发一个空许可,保持调用方释放语义统一。
            // (tokio 信号量上限远小于 usize::MAX,用 10^6 实用上限。)
            return match Arc::new(Semaphore::new(1_000_000)).try_acquire_owned() {
                Ok(permit) => AcquireOutcome::Acquired(permit),
                Err(_) => unreachable!("fresh semaphore must have capacity"),
            };
        }
        if aborted() {
            return AcquireOutcome::Rejected {
                status: 499,
                message: "Client connection closed before acquiring provider concurrency slot."
                    .to_owned(),
                aborted: true,
            };
        }
        let semaphore = self.queue(provider, config.max_in_flight_per_provider);
        if config.queue_timeout_ms <= 0 {
// 快速失败("do not queue" 模式)。
            match semaphore.clone().try_acquire_owned() {
                Ok(permit) => AcquireOutcome::Acquired(permit),
                Err(_) => AcquireOutcome::Rejected {
                    status: 429,
                    message: format!(
                        "Provider '{}' is at its in-flight limit ({}).",
                        provider.id, config.max_in_flight_per_provider
                    ),
                    aborted: false,
                },
            }
        } else {
            let wait = Duration::from_millis(config.queue_timeout_ms.max(0) as u64);
            match tokio::time::timeout(wait, semaphore.clone().acquire_owned()).await {
                Ok(Ok(permit)) => AcquireOutcome::Acquired(permit),
                Ok(Err(_)) => AcquireOutcome::Rejected {
                    status: 429,
                    message: format!("Provider '{}' concurrency pool closed.", provider.id),
                    aborted: false,
                },
                Err(_) => AcquireOutcome::Rejected {
                    status: 429,
                    message: format!(
                        "Provider '{}' is at its in-flight limit; queue wait timed out.",
                        provider.id
                    ),
                    aborted: false,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(max_in_flight: Option<i64>) -> Provider {
        let mut raw = serde_json::Map::new();
        raw.insert("id".into(), json!("prov-c"));
        raw.insert("api_base_url".into(), json!("http://127.0.0.1:1/c"));
        if let Some(cap) = max_in_flight {
            raw.insert("max_in_flight".into(), json!(cap));
        }
        Provider::from_raw(&raw).unwrap()
    }

    #[tokio::test]
    async fn fast_fails_at_limit() {
        let limiter = ConcurrencyLimiter::default();
        limiter.sync_config(&ConcurrencyConfig {
            enabled: true,
            max_in_flight_per_provider: 2,
            queue_timeout_ms: 0,
        });
        let provider = provider(None);
        let first = limiter.acquire(&provider, || false).await;
        let second = limiter.acquire(&provider, || false).await;
        assert!(first.ok() && second.ok());
        let third = limiter.acquire(&provider, || false).await;
        assert!(!third.ok());
        match third {
            AcquireOutcome::Rejected {
                status, aborted, ..
            } => {
                assert_eq!(status, 429);
                assert!(!aborted);
            }
            _ => panic!("expected rejection"),
        }
        // 释放一个槽位后可再申请。
        drop(first);
        assert!(limiter.acquire(&provider, || false).await.ok());
    }

    #[tokio::test]
    async fn provider_override_cap() {
        let limiter = ConcurrencyLimiter::default();
        limiter.sync_config(&ConcurrencyConfig {
            enabled: true,
            max_in_flight_per_provider: 8,
            queue_timeout_ms: 0,
        });
        let provider = provider(Some(1));
        // 许可必须持有(acquire 返回的临时值会在语句结束即释放)。
        let held = limiter.acquire(&provider, || false).await;
        assert!(held.ok());
        assert!(!limiter.acquire(&provider, || false).await.ok());
        drop(held);
    }

    #[tokio::test]
    async fn disabled_and_abort_paths() {
        let limiter = ConcurrencyLimiter::default();
        limiter.sync_config(&ConcurrencyConfig {
            enabled: false,
            max_in_flight_per_provider: 1,
            queue_timeout_ms: 0,
        });
        let provider = provider(None);
        assert!(limiter.acquire(&provider, || false).await.ok());
        assert!(limiter.acquire(&provider, || false).await.ok());

        limiter.sync_config(&ConcurrencyConfig {
            enabled: true,
            max_in_flight_per_provider: 1,
            queue_timeout_ms: 0,
        });
        let outcome = limiter.acquire(&provider, || true).await;
        match outcome {
            AcquireOutcome::Rejected {
                status, aborted, ..
            } => {
                assert_eq!(status, 499);
                assert!(aborted);
            }
            _ => panic!("expected abort rejection"),
        }
    }

    #[tokio::test]
    async fn queued_wait_times_out_to_429() {
        let limiter = ConcurrencyLimiter::default();
        limiter.sync_config(&ConcurrencyConfig {
            enabled: true,
            max_in_flight_per_provider: 1,
            queue_timeout_ms: 30,
        });
        let provider = provider(None);
        let held = limiter.acquire(&provider, || false).await;
        assert!(held.ok());
        let started = std::time::Instant::now();
        let outcome = limiter.acquire(&provider, || false).await;
        assert!(!outcome.ok());
        assert!(started.elapsed() >= Duration::from_millis(25));
    }
}
