//! 网关数据面(方案 #14 R2-R4)。
//!
//! 模块划分镜像 Python `application/gateway/`:
//! - [`auth`] -API Key 认证(`auth.py` 镜像);
//! - [`breaker`] -熔断器 + 状态持久化 + 半开(R4.8;纯状态机在 `wr_domain::gateway`);
//! - [`concurrency`] -每 provider 并发上限(`concurrency.py` 镜像);
//! - [`credential_pool`] -凭据池冷却与固定窗口计数(R4.7);
//! - [`limits`] -前置限流与预算(R4.8,`precheck.py` 镜像);
//! - [`idempotency`] -幂等重放缓存(R4.4);
//! - [`metrics`] -内存指标注册表(R4.9);
//! - [`events`] -管理面事件总线(R4.9,`/events` 端点随 R5);
//! - [`request_log`] -请求日志落库与 C3 门控(R4.5/R4.6);
//! - [`maintenance`] -日志保留/迁移后台任务(R4.11);
//! - [`session_affinity`] -会话粘性表 + 前缀钉扎表(20260905_03 S2);
//! - [`model_groups`] -聚合模型候选展开(20260905_04 §3.2);
//! - [`savings`] -节省动作解析(全局 → 规则两层覆盖,20260905_03);
//! - [`forward`] -转发管线(`forwarding.py` / `retry_fallback.py` 全量)。
//! - [`in_flight`] -在途请求注册表(调用日志 等待中/处理中;Rust 新增)。
//!
//! R4 在 R3 的路由/转换管线之上接上持久化与观测面:precheck 限流/预算 →
//! 幂等重放 → 凭据池账号注入 → 熔断持久化 → request_logs 落库(C3 三条策略)
//! 与用量入账 → 指标与事件流 → 后台保留清理。
//! 管理 RPC 与 `/events` 端点随 R5。

pub mod auth;
pub mod breaker;
pub mod concurrency;
pub mod credential_pool;
pub mod events;
pub mod forward;
pub mod idempotency;
pub mod in_flight;
pub mod limits;
pub mod maintenance;
pub mod metrics;
pub mod model_groups;
pub mod request_log;
pub mod response_cache;
pub mod savings;
pub mod session_affinity;

use std::sync::Arc;

use serde_json::{Value, json};

use wr_app::ports::GatewayStore;
use wr_domain::policy::PolicyEngine;
use wr_domain::provider::Provider;
use wr_domain::routing::config_compiler::compile_router_config;
use wr_domain::routing::contracts::CompiledRouterConfig;
use wr_infra::config::AppConfig;
use wr_infra::proxy::{ProxyDecision, describe_proxy_source, resolve_proxy_decision_with_override};
use wr_infra::sqlite_config::SqliteConfigSource;
use wr_infra::upstream::UpstreamClient;

use crate::settings::Settings;

use self::breaker::CircuitBreaker;
use self::concurrency::ConcurrencyLimiter;
use self::credential_pool::CredentialPool;
use self::events::{EventPublisher, EventSubscriber};
use self::idempotency::IdempotencyCache;
use self::limits::Prechecker;
use self::metrics::MetricsRegistry;
use self::request_log::{RequestLogEvent, RequestLogRecorder};
use wr_infra::sqlite::SqliteStore;

/// 已解析的配置快照(镜像 Python Container 的编译态配置)。
pub struct ConfigSnapshot {
    pub config: AppConfig,
    /// 解析成功的 provider 实体(api_key 已注入)。
    pub providers: Vec<Provider>,
    /// 任一 provider 记录解析失败时的镜像错误(Python `Provider.from_dict`
    /// 抛 ValueError → 请求 502 wrouter_error;黄金样本
    /// openai-responses-native-provider 钉住该行为)。
    pub provider_error: Option<String>,
    /// 路由编译产物(20261101_02 T3:仅存全局 fallback + 模型注册表;
    /// 规则编译与策略链已随规则域下线)。
    pub compiled: CompiledRouterConfig,
    /// R3:身份访问控制引擎(A3;空规则时恒按 default_effect)。
    pub access_policy: PolicyEngine,
}

impl ConfigSnapshot {
    pub fn load(source: &SqliteConfigSource) -> Result<Self, String> {
        let config = source.load_app_config()?;
        let mut providers = Vec::new();
        let mut provider_error = None;
        for record in &config.providers {
            match Provider::from_raw(&record.raw) {
                Ok(provider) => providers.push(provider),
                Err(message) => {
                    provider_error = Some(message);
                }
            }
        }
        Ok(Self::from_parts(config, providers, provider_error))
    }

    /// 由已解析部件编译完整快照(R3 路由/策略在此一次成型)。
    pub fn from_parts(
        config: AppConfig,
        providers: Vec<Provider>,
        provider_error: Option<String>,
    ) -> Self {
        // 路由编译输入:镜像 Python compile_router_config(cfg) 的 dict 形态
        // (20261101_02 T3:规则腿已删,注册表只吃 Providers/modelGroups,
        // fallback 单独传入)。
        let router_json = json!({
            "Providers": config
                .providers
                .iter()
                .map(|record| Value::Object(record.raw.clone()))
                .collect::<Vec<_>>(),
            "router_fallback": Value::Object(config.router_fallback.clone()),
            // 20260905_04:聚合实体进注册表(解析优先级见 model_registry)。
            "modelGroups": config.model_groups,
        });
        let compiled = compile_router_config(&router_json);
        let access_policy = wr_domain::policy::build_policy_engine(
            &config.policies.rules,
            config.policies.default_effect,
            config.policies.enabled,
        );
        Self {
            config,
            providers,
            provider_error,
            compiled,
            access_policy,
        }
    }
}

/// 网关共享依赖(镜像 Python `GatewayApplication` 的协作对象子集)。
pub struct GatewayDeps {
    pub settings: Arc<Settings>,
    pub config_source: Arc<SqliteConfigSource>,
    pub upstream: UpstreamClient,
    /// R4.8:熔断状态板(sqlite 持久化 + 每 provider 覆盖 + 半开)。
    pub breaker: CircuitBreaker,
    pub concurrency: ConcurrencyLimiter,
    /// R4:存储门面的端口面(组合端口)。
    pub store: Arc<dyn GatewayStore>,
    /// R4.5/R4.6:请求日志落库执行器。
    pub log_recorder: RequestLogRecorder,
    /// R4.7:凭据池运行态。
    pub credentials: CredentialPool,
    /// R4.8:前置限流与预算。
    pub prechecker: Prechecker,
    /// R4.4:幂等重放缓存。
    pub idempotency: IdempotencyCache,
    /// R4.9:内存指标注册表。
    pub metrics: Arc<MetricsRegistry>,
    /// R4.9:事件发布端(R5 的 `/events` 订阅它)。
    pub events: EventPublisher,
    /// 在途请求注册表(调用日志 等待中/处理中;Rust 新增)。
    pub in_flight: in_flight::InFlightRegistry,
    /// 20260905_03 S2:会话粘性表(键 = K4 会话键;TTL 随 savings 配置)。
    pub session_affinity: session_affinity::SessionAffinityTable,
    /// 20260905_04 M7:聚合 sticky 策略粘性表(键 = `model-group:{组id}:
    /// {会话键}`;TTL 与会话粘性同源)。
    pub group_sticky: session_affinity::SessionAffinityTable,
    /// 20260905_03 S2:前缀钉扎表(键 = 前缀 SHA-256;TTL 固定 300s)。
    pub prefix_pins: session_affinity::PrefixPinTable,
    /// 20260905_03 S6:响应缓存内存层(磁盘层走 `store` 端口)。
    pub response_cache: response_cache::ResponseCache,
    event_subscriber: std::sync::Mutex<Option<EventSubscriber>>,
    snapshot: tokio::sync::RwLock<Option<Arc<ConfigSnapshot>>>,
}

impl GatewayDeps {
    /// 装配网关依赖。`store` 是 R4 的读写门面(与只读的 `config_source`
    /// 指向同一个 `config.sqlite`)。
    pub fn new(
        settings: Arc<Settings>,
        config_source: Arc<SqliteConfigSource>,
        store: Arc<SqliteStore>,
    ) -> Self {
        let gateway_store: Arc<dyn GatewayStore> = store.clone();
        let (events, subscriber) = EventPublisher::new();
        let metrics = Arc::new(MetricsRegistry::new());
        let (log_recorder, drainer) =
            RequestLogRecorder::new(store, metrics.clone(), Some(events.clone()));
        // 落库 drain 任务:请求路径只入队,写序仍是入队序(单写者)。
        tokio::spawn(drainer.run());
        Self {
            settings,
            config_source,
            upstream: UpstreamClient::new(),
            breaker: CircuitBreaker::new(Some(gateway_store.clone())),
            concurrency: ConcurrencyLimiter::default(),
            store: gateway_store.clone(),
            log_recorder,
            credentials: CredentialPool::new(),
            prechecker: Prechecker::default(),
            idempotency: IdempotencyCache::new(
                Some(gateway_store),
                wr_infra::config::IdempotencyConfig::default(),
            ),
            metrics,
            events,
            in_flight: in_flight::InFlightRegistry::default(),
            session_affinity: session_affinity::SessionAffinityTable::default(),
            group_sticky: session_affinity::SessionAffinityTable::default(),
            prefix_pins: session_affinity::PrefixPinTable::default(),
            response_cache: response_cache::ResponseCache::default(),
            event_subscriber: std::sync::Mutex::new(Some(subscriber)),
            snapshot: tokio::sync::RwLock::new(None),
        }
    }

    /// 取走事件订阅端(管理面 `/events` 用;只能取一次)。
    pub fn take_event_subscriber(&self) -> Option<EventSubscriber> {
        self.event_subscriber
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    /// 订阅一个独立 SSE 客户端通道(`take_event_subscriber` 仅消费第一次调用
    /// 用于守护任务;其余连接走 `subscribe`)。
    pub fn subscribe_event(&self) -> EventSubscriber {
        self.events.subscribe()
    }

    /// 配置保存后失效进程内快照(镜像 Python `reload_config`/`reload_reliability`):
    /// 下一次 `snapshot()` 重新从 sqlite 装载,prechecker/idempotency 配置随之更新。
    pub async fn invalidate_snapshot(&self) {
        *self.snapshot.write().await = None;
        if let Ok(snap) = self.snapshot().await {
            self.sync_runtime_config(&snap);
        }
    }

    /// 把一次配置同步到各运行态组件(镜像 Python 的 `update_config` 链)。
    pub fn sync_runtime_config(&self, snapshot: &ConfigSnapshot) {
        let reliability = &snapshot.config.reliability;
        self.prechecker.update_config(&reliability.precheck);
        self.idempotency.update_config(&reliability.idempotency);
        // 20260905_03:粘性 TTL 随全局 savings 配置刷新(表本身无全局开关,
        // 仅规则 stickySession 触发;前缀钉扎 TTL 固定 300s 不同步)。
        self.session_affinity
            .sticky_set_ttl_ms(snapshot.config.savings.sticky.ttl_secs * 1000);
        // 20260905_04 M7:聚合 sticky 粘性表 TTL 与会话粘性同源。
        self.group_sticky
            .sticky_set_ttl_ms(snapshot.config.savings.sticky.ttl_secs * 1000);
        // 20260905_03 S6:响应缓存内存层容量随配置刷新。
        response_cache::sync_limits(
            &self.response_cache,
            &snapshot.config.savings.response_cache,
        );
    }

    /// 等日志队列排空(停机与测试用)。
    pub async fn flush_request_logs(&self) {
        self.log_recorder.flush().await;
    }

    /// 停机收尾:排空后关闭队列。
    pub async fn shutdown_request_logs(&self) {
        self.log_recorder.flush().await;
        self.log_recorder.shutdown();
    }

    /// 请求日志:tracing 输出 + 落库入队(C3 门控在 drain 侧判)。
    pub fn record_log(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        request_id: &str,
        event: &RequestLogEvent,
    ) {
        self.record_log_extra(
            snapshot,
            request_id,
            event,
            request_log::RequestLogExtra::default(),
        );
    }

    /// 在途快照(调用日志页与正式行合并展示;供 admin 层)。
    pub fn in_flight_rows(&self) -> Vec<Value> {
        self.in_flight.snapshot()
    }

    /// 为「从未触达 provider」的请求落一条审计行(镜像 `record_rejected`)。
    ///
    /// `provider` 为空 ⇒ 该行留在 request_logs 供审计,但被用量聚合的
    /// 「有效请求」判定排除。`payload=None` ⇒ 不写 preview。
    #[allow(clippy::too_many_arguments)]
    pub fn record_rejected(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        protocol: &str,
        model: &str,
        request_id: &str,
        status: u16,
        end_reason: &'static str,
        error: Option<&str>,
    ) {
        let mut extra = serde_json::Map::new();
        if let Some(message) = error {
            extra.insert("error".to_owned(), json!(message));
        }
        let model = if model.is_empty() {
            "unknown".to_owned()
        } else {
            model.to_owned()
        };
        self.record_log_extra(
            snapshot,
            request_id,
            &RequestLogEvent {
                protocol: protocol.to_owned(),
                model,
                status,
                stream: false,
                duration_ms: 0.0,
                provider_attempts: Vec::new(),
                retry_attempts: Vec::new(),
                end_reason,
                provider: None,
                account: None,
                api_key_id: None,
                first_byte_ms: None,
                error: None,
            },
            request_log::RequestLogExtra {
                extra,
                ..Default::default()
            },
        );
    }

    /// 带扩展事实(用量入账 / 正文捕获 / 请求元信息)的入口。
    ///
    /// 同时是在途注册表的**落库汇点**:取走该请求的条目(等待中/处理中
    /// 生命周期随此结束),并把暂存的思考强度并入 extra(键
    /// `reasoning_effort`;登记时已从客户端原始请求体提取)。
    pub fn record_log_extra(
        &self,
        snapshot: &Arc<ConfigSnapshot>,
        request_id: &str,
        event: &RequestLogEvent,
        mut extra: request_log::RequestLogExtra,
    ) {
        if let Some(entry) = self.in_flight.remove(request_id)
            && let Some(effort) = entry.reasoning_effort
            && !extra.extra.contains_key("reasoning_effort")
        {
            extra
                .extra
                .insert("reasoning_effort".to_owned(), json!(effort));
        }
        // 请求时刻的有效上游线格式(payload 键 `provider_protocol`):转换
        // 链路必须钉在请求发生时的配置上。快照是请求开始时捕获的,读取
        // 侧优先消费该存量值;旧行/从未触达 provider 的行(无键)才按当前
        // 配置现算。否则用户改供应商 type 后,历史行链路全部跟着漂移
        // (2026-10-14)。
        if let Some(provider_id) = event.provider.as_deref()
            && !extra.extra.contains_key("provider_protocol")
            && let Some(provider) = snapshot.providers.iter().find(|item| item.id == provider_id)
        {
            extra.extra.insert(
                "provider_protocol".to_owned(),
                json!(provider.effective_wire_protocol()),
            );
        }
        emit_request_log(event);
        self.log_recorder
            .enqueue_extra(snapshot, request_id, event, extra);
    }

    /// 取(或 lazy 装载)配置快照。装载失败返回镜像错误字符串。
    pub async fn snapshot(&self) -> Result<Arc<ConfigSnapshot>, String> {
        {
            let cached = self.snapshot.read().await;
            if let Some(snapshot) = cached.as_ref() {
                return Ok(snapshot.clone());
            }
        }
        let mut write = self.snapshot.write().await;
        if let Some(snapshot) = write.as_ref() {
            return Ok(snapshot.clone());
        }
        let snapshot = Arc::new(ConfigSnapshot::load(&self.config_source)?);
        *write = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// 出站代理解析(每次请求基于快照 `config.proxy` + 供应商级覆盖;
    /// 方案 20260906_01 B4)。
    pub fn proxy_decision_for(
        snapshot: &ConfigSnapshot,
        provider: &Provider,
        url: &str,
    ) -> ProxyDecision {
        resolve_proxy_decision_with_override(
            Some(&snapshot.config.proxy),
            provider.proxy.as_ref(),
            url,
        )
    }

    /// 请求日志用的代理来源标签(方案 §4.7;凭据一律不落日志,决策 #10)。
    pub fn proxy_source_label(snapshot: &ConfigSnapshot, decision: &ProxyDecision) -> String {
        describe_proxy_source(Some(&snapshot.config.proxy), decision)
    }
}

/// 便捷共享句柄类型。
pub type SharedGatewayDeps = Arc<GatewayDeps>;

/// R2.7 最小请求日志:字段与 Python `request_log.py` 的 payload 对齐。
/// 落库由 [`GatewayDeps::record_log`] 接续。
fn emit_request_log(event: &RequestLogEvent) {
    tracing::info!(
        target: "wrouter.gateway",
        protocol = %event.protocol,
        model = %event.model,
        status = event.status,
        stream = event.stream,
        duration_ms = event.duration_ms,
        provider_attempts = ?event.provider_attempts,
        end_reason = %event.end_reason,
        provider = event.provider.as_deref().unwrap_or(""),
        api_key_id = event.api_key_id.as_deref().unwrap_or(""),
        first_byte_ms = event.first_byte_ms.unwrap_or(0.0),
        error = event.error.as_deref().unwrap_or(""),
        "gateway_request",
    );
}
