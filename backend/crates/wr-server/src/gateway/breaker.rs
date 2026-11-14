//! 应用层熔断器(方案 #14 R4.8;镜像 Python `application/gateway/
//! circuit_breaker.py`,纯状态机已迁到 `wr_domain::gateway`)。
//!
//! 相比 R2 的最小版(`BreakerBoard`),R4 补齐三件事:
//! 1. **SQLite 持久化**:只在**状态跃迁**时落库(`record_failure` 仅在结果
//!    为 OPEN、或跃迁前是 HALF_OPEN 时写;`record_success` 仅在跃迁前不是
//!    CLOSED 时写)——避免每次失败都写一次库的写放大;
//! 2. **每 provider 覆盖**:`provider.failure_threshold` / `cooldown_ms`
//!    大于 0 时优先于全局 `reliability.circuit_breaker`;
//! 3. **半开语义**:冷却到期放行试探请求,再失败立即以全新冷却重开。
//!
//! 失败计次口径仍是 C4:只有 `failure_status_codes`(默认 408/429/500/
//! 502/503/504,**不含 501**)计入熔断;其余状态码一律按成功清零计数。
//!
//! ## 时钟基
//! Python 持久化 `time.monotonic()*1000`(开机起算),Rust 用 epoch 毫秒。
//! 迁移期同一数据目录可能被双方读写,因此 [`CircuitBreaker::reload`] 用
//! [`normalize_future_ms`] 把「明显不属于本时钟基」的开闸时刻按已到期处理:
//! 最坏代价是多放一次试探请求,好处是绝不会把 provider 永久锁死。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wr_domain::gateway::{
    BreakerStatus, CircuitBreakerState, compute_status, record_failure as domain_record_failure,
    record_success as domain_record_success,
};
use wr_domain::observability::CircuitBreakerSnapshot;
use wr_domain::provider::Provider;
use wr_infra::config::CircuitBreakerConfig;
use wr_infra::sqlite::engine::{normalize_future_ms, wall_ms};

use wr_app::ports::{BreakerStateRow, GatewayStore};

/// 每 provider 熔断状态板 + 跃迁持久化。
pub struct CircuitBreaker {
    store: Option<Arc<dyn GatewayStore>>,
    states: Mutex<HashMap<String, CircuitBreakerState>>,
    /// 时钟注入点(毫秒);默认 epoch 毫秒。
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(None)
    }
}

impl std::fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.states.lock().map_or(0, |guard| guard.len());
        f.debug_struct("CircuitBreaker")
            .field("providers", &count)
            .field("persistent", &self.store.is_some())
            .finish()
    }
}

impl CircuitBreaker {
    /// `store = None` 时退化为纯内存熔断(单测与无持久化场景)。
    pub fn new(store: Option<Arc<dyn GatewayStore>>) -> Self {
        Self {
            store,
            states: Mutex::new(HashMap::new()),
            clock: Arc::new(wall_ms),
        }
    }

    /// 覆盖时钟源(测试用固定时钟驱动半开/重开,不必真等冷却)。
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    fn now_ms(&self) -> i64 {
        (self.clock)()
    }

    /// 从库里重载全部状态(镜像 Python `reload()`)。启动时调一次;
    /// 配置热更新后再调可以看到兄弟进程的写入。
    pub async fn reload(&self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let Ok(rows) = store.load_all_breaker_states().await else {
            return;
        };
        let now = self.now_ms();
        // 可信未来上限 = 一年:超过它的一律视为外来时钟基(已到期)。
        const HORIZON_MS: i64 = 365 * 24 * 3600 * 1000;
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.clear();
        for (provider_id, row) in rows {
            states.insert(
                provider_id,
                CircuitBreakerState {
                    consecutive_failures: row.consecutive_failures,
                    opened_until_ms: normalize_future_ms(row.opened_until_ms, now, HORIZON_MS),
                    last_state_change_ms: row.last_state_change_ms,
                },
            );
        }
    }

    /// 熔断是否打开(冷却到期视为半开放行)。
    ///
    /// `config` 由调用方从当次配置快照给出(等价 Python 里 `update_config`
    /// 推进进来的同一份);`provider` 用于读每 provider 覆盖。
    pub fn is_open(
        &self,
        provider_id: &str,
        provider: Option<&Provider>,
        config: &CircuitBreakerConfig,
    ) -> bool {
        if !config.enabled {
            return false;
        }
        let (threshold, cooldown) = thresholds(config, provider);
        let now = self.now_ms();
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let state = states.entry(provider_id.to_owned()).or_default();
        compute_status(state, now, threshold, cooldown) == BreakerStatus::Open
    }

    /// 上游响应计次:命中 `failure_status_codes` 才算失败,其余按成功。
    pub async fn record_response(
        &self,
        provider_id: &str,
        status: u16,
        provider: Option<&Provider>,
        config: &CircuitBreakerConfig,
    ) {
        if !config.enabled {
            return;
        }
        let failed = config
            .failure_status_codes
            .iter()
            .any(|code| *code == i64::from(status));
        if failed {
            self.record_failure(provider_id, provider, config).await;
        } else {
            self.record_success(provider_id, provider, config).await;
        }
    }

    /// 网络层失败(连接/超时)计次。
    pub async fn record_failure(
        &self,
        provider_id: &str,
        provider: Option<&Provider>,
        config: &CircuitBreakerConfig,
    ) {
        if !config.enabled {
            return;
        }
        let (threshold, cooldown) = thresholds(config, provider);
        let now = self.now_ms();
        let to_persist = {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let state = states.entry(provider_id.to_owned()).or_default();
            let before = compute_status(state, now, threshold, cooldown);
            let after = domain_record_failure(state, now, threshold, cooldown);
            (after == BreakerStatus::Open || before == BreakerStatus::HalfOpen).then_some(*state)
        };
        if let Some(state) = to_persist {
            self.persist(provider_id, &state).await;
        }
    }

    pub async fn record_success(
        &self,
        provider_id: &str,
        provider: Option<&Provider>,
        config: &CircuitBreakerConfig,
    ) {
        if !config.enabled {
            return;
        }
        let (threshold, cooldown) = thresholds(config, provider);
        let now = self.now_ms();
        let to_persist = {
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let state = states.entry(provider_id.to_owned()).or_default();
            let before = compute_status(state, now, threshold, cooldown);
            domain_record_success(state, now);
            (before != BreakerStatus::Closed).then_some(*state)
        };
        if let Some(state) = to_persist {
            self.persist(provider_id, &state).await;
        }
    }

    /// 清掉一个 provider 的熔断状态(管理面 reset 用)。
    pub fn reset(&self, provider_id: &str) {
        if let Ok(mut states) = self.states.lock() {
            states.remove(provider_id);
        }
    }

    /// 只读视图(`provider_health()` 与 503 `breaker_open` 的 details)。
    pub fn snapshot(
        &self,
        provider_id: &str,
        provider: Option<&Provider>,
        config: &CircuitBreakerConfig,
    ) -> CircuitBreakerSnapshot {
        let (threshold, cooldown) = thresholds(config, provider);
        let now = self.now_ms();
        let state = self
            .states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider_id)
            .copied()
            .unwrap_or_default();
        CircuitBreakerSnapshot {
            provider_id: provider_id.to_owned(),
            state: compute_status(&state, now, threshold, cooldown)
                .as_str()
                .to_owned(),
            failures: state.consecutive_failures,
            opened_until_ms: state.opened_until_ms,
        }
    }

    async fn persist(&self, provider_id: &str, state: &CircuitBreakerState) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let row = BreakerStateRow {
            consecutive_failures: state.consecutive_failures,
            opened_until_ms: state.opened_until_ms,
            last_state_change_ms: state.last_state_change_ms,
        };
        if let Err(error) = store.save_breaker_state(provider_id, &row).await {
            // 持久化失败不改本次判定(内存仍是权威),只告警。
            tracing::warn!(target: "wrouter.gateway.breaker", provider = %provider_id, %error,
                           "breaker_state_persist_failed");
        }
    }
}

/// `(failure_threshold, cooldown_ms)`:每 provider 覆盖优先(> 0 才算覆盖)。
fn thresholds(config: &CircuitBreakerConfig, provider: Option<&Provider>) -> (i64, i64) {
    let mut threshold = config.failure_threshold.max(1);
    let mut cooldown = config.cooldown_ms.max(1);
    if let Some(provider) = provider {
        if provider.failure_threshold.is_some_and(|value| value > 0) {
            threshold = provider.failure_threshold.unwrap_or(threshold);
        }
        if provider.cooldown_ms.is_some_and(|value| value > 0) {
            cooldown = provider.cooldown_ms.unwrap_or(cooldown);
        }
    }
    (threshold, cooldown)
}

/// 503「breaker open」响应体(镜像 `breaker_failure_response`,形状对齐
/// 熔断详情字段)。
pub fn breaker_failure_response(provider_id: &str, snapshot: &CircuitBreakerSnapshot) -> Value {
    json!({
        "error": {
            "type": "breaker_open",
            "message": format!("Provider {provider_id} circuit breaker is open."),
            "code": "circuit_breaker_open",
            "provider": provider_id,
            "details": snapshot.to_dict(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use wr_infra::sqlite::SqliteStore;

    fn config(threshold: i64, cooldown_ms: i64) -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            enabled: true,
            failure_threshold: threshold,
            cooldown_ms,
            failure_status_codes: vec![408, 429, 500, 502, 503, 504],
        }
    }

    fn provider_raw(id: &str, extra: Vec<(&str, Value)>) -> Provider {
        let mut raw = serde_json::Map::new();
        raw.insert("id".to_owned(), json!(id));
        raw.insert("api_base_url".to_owned(), json!("http://127.0.0.1:1/p"));
        for (key, value) in extra {
            raw.insert(key.to_owned(), value);
        }
        Provider::from_raw(&raw).unwrap()
    }

    /// 可控时钟(测试里推进时间而不是 sleep)。
    struct Tick(Arc<AtomicI64>);

    impl Tick {
        fn new(start: i64) -> Self {
            Self(Arc::new(AtomicI64::new(start)))
        }
        fn advance(&self, ms: i64) {
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
        fn clock(&self) -> Arc<dyn Fn() -> i64 + Send + Sync> {
            let inner = self.0.clone();
            Arc::new(move || inner.load(Ordering::SeqCst))
        }
    }

    fn breaker_with_clock(clock: Arc<dyn Fn() -> i64 + Send + Sync>) -> CircuitBreaker {
        CircuitBreaker {
            store: None,
            states: Mutex::new(HashMap::new()),
            clock,
        }
    }

    /// C4 + 半开:阈值 3 → 开闸;冷却内 OPEN;到期 HALF_OPEN 放行;
    /// 试探再失败立即以全新冷却重开。
    #[tokio::test]
    async fn half_open_trial_failure_reopens_immediately() {
        let tick = Tick::new(1_000);
        let breaker = breaker_with_clock(tick.clock());
        let config = config(3, 500);
        let provider = provider_raw("p1", vec![]);
        for _ in 0..2 {
            breaker.record_failure("p1", Some(&provider), &config).await;
        }
        assert!(
            !breaker.is_open("p1", Some(&provider), &config),
            "两次失败未到阈值"
        );
        breaker.record_failure("p1", Some(&provider), &config).await;
        assert!(breaker.is_open("p1", Some(&provider), &config));
        assert_eq!(
            breaker.snapshot("p1", Some(&provider), &config).state,
            "open"
        );
        tick.advance(400);
        assert!(
            breaker.is_open("p1", Some(&provider), &config),
            "冷却未到期"
        );
        tick.advance(200);
        assert!(
            !breaker.is_open("p1", Some(&provider), &config),
            "冷却到期必须半开放行试探请求"
        );
        assert_eq!(
            breaker.snapshot("p1", Some(&provider), &config).state,
            "half_open"
        );
        breaker.record_failure("p1", Some(&provider), &config).await;
        assert!(
            breaker.is_open("p1", Some(&provider), &config),
            "半开再失败必须立即重开(再等一个完整冷却)"
        );
    }

    #[tokio::test]
    async fn success_closes_and_snapshot_reports_closed() {
        let tick = Tick::new(1_000);
        let breaker = breaker_with_clock(tick.clock());
        let config = config(1, 500);
        breaker.record_failure("p1", None, &config).await;
        assert!(breaker.is_open("p1", None, &config));
        breaker.record_success("p1", None, &config).await;
        let snapshot = breaker.snapshot("p1", None, &config);
        assert_eq!(snapshot.state, "closed");
        assert_eq!(snapshot.failures, 0);
        assert_eq!(snapshot.opened_until_ms, 0);
    }

    /// 每 provider 覆盖:全局阈值 5 不触发,provider 阈值 1 立即开闸。
    #[tokio::test]
    async fn per_provider_threshold_override_wins() {
        let tick = Tick::new(0);
        let breaker = breaker_with_clock(tick.clock());
        let config = config(5, 1000);
        let strict = provider_raw("p2", vec![("failure_threshold", json!(1))]);
        breaker.record_failure("p2", Some(&strict), &config).await;
        assert!(
            breaker.is_open("p2", Some(&strict), &config),
            "provider 级 failure_threshold=1 必须压过全局 5"
        );
        let relaxed = provider_raw("p3", vec![("failure_threshold", json!(0))]);
        breaker.record_failure("p3", Some(&relaxed), &config).await;
        assert!(
            !breaker.is_open("p3", Some(&relaxed), &config),
            "0 不是有效覆盖,按全局阈值走"
        );
    }

    /// C4:默认失败码不含 501 → 反复 501 永不累计到开闸。
    #[tokio::test]
    async fn non_failure_status_resets_counter() {
        let tick = Tick::new(0);
        let breaker = breaker_with_clock(tick.clock());
        let config = config(2, 1000);
        for _ in 0..10 {
            breaker.record_response("p1", 501, None, &config).await;
        }
        assert!(
            !breaker.is_open("p1", None, &config),
            "501 不在 failure_status_codes 里,必须按成功清零"
        );
        assert_eq!(breaker.snapshot("p1", None, &config).failures, 0);
    }

    #[tokio::test]
    async fn disabled_breaker_never_opens_or_persists() {
        let tick = Tick::new(0);
        let breaker = breaker_with_clock(tick.clock());
        let config = CircuitBreakerConfig {
            enabled: false,
            ..config(1, 1000)
        };
        breaker.record_failure("p1", None, &config).await;
        assert!(!breaker.is_open("p1", None, &config));
        assert_eq!(breaker.snapshot("p1", None, &config).failures, 0);
    }

    /// R4.8 持久化:开闸跃迁落库,reload 后仍判定为开(同一时钟基)。
    #[tokio::test]
    async fn trip_persists_and_reload_restores() {
        let dir = std::env::temp_dir().join(format!("wr-r4-breaker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(SqliteStore::open(&dir).await.unwrap());
        let gateway_store: Arc<dyn GatewayStore> = store.clone();
        let config = config(1, 60_000);
        let breaker = CircuitBreaker::new(Some(gateway_store.clone()));
        breaker.record_failure("p9", None, &config).await;
        let row = gateway_store
            .load_breaker_state("p9")
            .await
            .unwrap()
            .expect("开闸必须落库");
        assert_eq!(row.consecutive_failures, 1);
        assert!(row.opened_until_ms > 0);

        // 新实例(模拟重启)从库里恢复同样的判定。
        let restored = CircuitBreaker::new(Some(gateway_store));
        restored.reload().await;
        assert!(
            restored.is_open("p9", None, &config),
            "重启后仍应处于冷却中"
        );
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 未跃迁的失败计次不写库(Python 的写放大保护)。
    #[tokio::test]
    async fn no_persist_until_transition() {
        let dir = std::env::temp_dir().join(format!("wr-r4-breaker-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(SqliteStore::open(&dir).await.unwrap());
        let gateway_store: Arc<dyn GatewayStore> = store.clone();
        let breaker = CircuitBreaker::new(Some(gateway_store.clone()));
        let config = config(3, 1000);
        breaker.record_failure("q", None, &config).await;
        assert!(
            gateway_store
                .load_breaker_state("q")
                .await
                .unwrap()
                .is_none(),
            "未到阈值的失败不该写库"
        );
        breaker.record_success("q", None, &config).await;
        assert!(
            gateway_store
                .load_breaker_state("q")
                .await
                .unwrap()
                .is_none(),
            "本就 CLOSED 的成功不该写库"
        );
        store.close().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn foreign_clock_base_opened_until_is_normalized() {
        // Python 写的 monotonic 值(开机起算)在 epoch 毫秒视角里是远古值
        // → normalize 后按已到期(半开)处理,不会把 provider 锁死。
        assert_eq!(normalize_future_ms(36_000_000, 1_700_000_000_000, 1000), 0);
        // 本运行时写入、仍在可信冷却区间内 → 原样保留。
        assert_eq!(
            normalize_future_ms(1_700_000_030_000, 1_700_000_000_000, 60_000),
            1_700_000_030_000
        );
        // 落在未来但超出可信 horizon(Rust 行被别的时钟基读到)→ 归零。
        assert_eq!(
            normalize_future_ms(9_000_000_000_000_000, 1_700_000_000_000, 1000),
            0
        );
    }

    #[test]
    fn failure_response_shape() {
        let snapshot = CircuitBreakerSnapshot {
            provider_id: "p1".to_owned(),
            state: "open".to_owned(),
            failures: 3,
            opened_until_ms: 42,
        };
        let body = breaker_failure_response("p1", &snapshot);
        assert_eq!(body["error"]["type"], json!("breaker_open"));
        assert_eq!(body["error"]["code"], json!("circuit_breaker_open"));
        assert_eq!(
            body["error"]["message"],
            json!("Provider p1 circuit breaker is open.")
        );
        assert_eq!(body["error"]["details"]["breaker_state"], json!("open"));
        assert_eq!(body["error"]["details"]["open_until"], json!(42));
    }
}
