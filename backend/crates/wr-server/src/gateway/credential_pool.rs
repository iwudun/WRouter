//! 凭据池运行态(方案 #14 R4.7;镜像 Python `application/gateway/
//! credential_pool.py`)。
//!
//! 进程内状态:每 `(provider, account)` 的冷却 + 固定窗口计数。
//! Python 侧刻意用进程内存(重启清冷却,与上游 credential-pool.ts 一致);
//! Rust 同样是进程内,但把 `threading.Lock` 换成 `Mutex`,在 tokio 多线程
//! 运行时下天然共享(这是对 Python 的架构改进,行为语义不变)。
//!
//! 权威池行仍是 sqlite 的 `provider_accounts`(`secret_store.rs`),这里只是
//! 运行态叠加层。
//!
//! ## 时钟
//! Python 的冷却用 `time.time()*1000`(**epoch 毫秒**,与熔端的 monotonic
//! 不同),Rust 用同一个 epoch 毫秒时钟,两边数值可直接互读。
//!
//! ## 接线现状(方案 20260905_02 K2/K3 更新,2026-09-06)
//! 上游 Python 侧 `record_outcome` / `increment_counter` / `counter_value` /
//! `clear_cooldown` / `cooldown_reason` **没有生产调用点**(只有集成测试);
//! 唯一活着的路径是 `ProviderSelector.select_provider → _pick_account` 读
//! [`CredentialPool::is_cooled_down`]。Rust 侧自 K3 起为**行为变更**(偏差
//! 登记,无 Python 镜像):
//! - `record_outcome` 已在网关转发路径激活(`forward.rs` 非流式上游响应
//!   返回处 + 流式**握手期**失败;SSE 开始后的中途断连不记录);
//! - 冷却时长分档:401/403 → 300s([`CREDENTIAL_COOLDOWN_MS`]),429/5xx →
//!   60s(默认);429/5xx 可被上游 `Retry-After` 覆盖(min(值, 24h),K8 还
//!   支持从 429 错误正文做 reset 文本解析兜底);`should_cooldown` 触发码
//!   集合保持不变(401/403/429/5xx);
//! - `pick_account` 策略化(first/pinned/weighted/sticky,方案 §3.2),
//!   weighted/sticky 与 `WeightedRng`/`sticky_score_u`(wr-domain)为
//!   Rust 新增能力,非 Python 镜像;
//! - `clear_cooldown` 由管理面 RPC `clearCredentialCooldown` 调用(K5);
//! - `increment_counter` / `counter_value` 仍无生产调用点(端口面保留)。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use wr_domain::provider::Provider;
use wr_domain::provider::ProviderAccount;
use wr_domain::provider::ProviderCredentialStrategy;
use wr_domain::rest::{FailureSignal, RestClass, rest_duration_ms};
use wr_infra::sqlite::engine::wall_ms;

/// 失败后的默认冷却(毫秒),与上游一致。
pub const DEFAULT_COOLDOWN_MS: i64 = 60_000;

/// 触发 60s 冷却的上游状态码。
pub const COOLDOWN_STATUS_CODES: &[i64] = &[401, 403, 429];

/// 401/403 的凭据级失效冷却(毫秒):60s 内必然反复撞,升到 300s
/// (方案 20260905_02 §3.2 分档表)。
pub const CREDENTIAL_COOLDOWN_MS: i64 = 300_000;

/// `Retry-After` 冷却上限(毫秒,24h):防恶意 Retry-After 把 key 锁死。
pub const MAX_RETRY_AFTER_COOLDOWN_MS: i64 = 86_400_000;

/// 阶段五 C-2:冷却到期抖动的上限(毫秒)。
///
/// 同批冷却的账号原本会在同一毫秒集体恢复 → 上游瞬间被同一批请求
/// 再打穿。抖动把到期时刻按账号摊开;幅度取「时长的 1/5」且不超过
/// 30s(太长的抖动会让冷却失去意义)。
pub const MAX_COOLDOWN_JITTER_MS: i64 = 30_000;

/// 冷却表键(账号级):`{provider_id}::{account_id}`。
pub fn state_key(provider_id: &str, account_id: &str) -> String {
    format!("{provider_id}::{account_id}")
}

/// 冷却表键(账号+模型级):`{provider_id}::{account_id}::{model}`。
///
/// 阶段五 C-2:限速/上游故障这类**与模型无关也不该封整把 key** 的失败
/// 只在模型维度计数 —— 同一账号的其它模型仍可用。
pub fn model_state_key(provider_id: &str, account_id: &str, model: &str) -> String {
    format!("{provider_id}::{account_id}::{model}")
}

/// 阶段五 C-2:冷却到期抖动(毫秒)。
///
/// 按种子(账号+模型)确定性派生:同一键恒得同值(可测、进程间一致),
/// 不同键分散在 `[0, 幅度]` 区间内。
pub fn cooldown_jitter_ms(seed: &str, duration_ms: i64) -> i64 {
    if duration_ms <= 0 {
        return 0;
    }
    let span = (duration_ms / 5).min(MAX_COOLDOWN_JITTER_MS).max(1);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // 末位再混一次(splitmix64 的 finalizer),避免相邻种子挨在一起。
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    (hash % (span as u64 + 1)) as i64
}

/// 无模型维度时的默认失败类别(按状态码;语义细分由读侧分类器给出)。
pub fn default_rest_class(status_code: i64) -> RestClass {
    match status_code {
        401 | 403 => RestClass::Auth,
        429 => RestClass::RateLimit,
        code if code >= 500 => RestClass::ServerError,
        _ => RestClass::Unknown,
    }
}

/// 计数表键:`{provider_id}::{account_id}::{rule_name}::{window_start_ms}`。
pub fn counter_key(
    provider_id: &str,
    account_id: &str,
    rule_name: &str,
    window_start_ms: i64,
) -> String {
    format!("{provider_id}::{account_id}::{rule_name}::{window_start_ms}")
}

/// 该状态码是否应让凭据进冷却。
pub fn should_cooldown(status_code: i64) -> bool {
    COOLDOWN_STATUS_CODES.contains(&status_code) || status_code >= 500
}

/// K3 分档冷却(毫秒):401/403 → 300s(凭据级失效);429 / >=500 → 60s
/// (限速瞬时,用池默认);其余 → `None`(不冷却;2xx–4xx 非冷却码仍走
/// 清冷却)。`should_cooldown` 的触发码集合保持不变。
pub fn cooldown_tier(status_code: i64) -> Option<i64> {
    match status_code {
        401 | 403 => Some(CREDENTIAL_COOLDOWN_MS),
        429 => Some(DEFAULT_COOLDOWN_MS),
        s if s >= 500 => Some(DEFAULT_COOLDOWN_MS),
        _ => None,
    }
}

/// 「2xx–4xx 且不是 auth/rate/server」——上游把它当作「用过,清冷却」。
pub fn status_in_2xx_to_4xx(status_code: i64) -> bool {
    (200..500).contains(&status_code) && !COOLDOWN_STATUS_CODES.contains(&status_code)
}

#[derive(Debug, Clone)]
struct Cooldown {
    reason: String,
    until_ms: i64,
}

/// K5 观测:每键最近一次 `record_outcome` 的状态码(与冷却态独立,
/// 2xx 清冷却后仍保留)。数量受 (provider, account) 配置上界约束,不清理。
#[derive(Debug, Clone, Copy)]
struct LastOutcome {
    status_code: i64,
}

#[derive(Debug, Clone, Copy)]
struct CounterBucket {
    window_start_ms: i64,
    value: i64,
}

/// 线程安全的每凭据冷却 + 计数存储。一个进程一个实例。
#[derive(Default)]
pub struct CredentialPool {
    cooldowns: Mutex<HashMap<String, Cooldown>>,
    counters: Mutex<HashMap<String, CounterBucket>>,
    /// K5:每键最近一次上游结果(观测面,只读消费)。
    last_outcomes: Mutex<HashMap<String, LastOutcome>>,
    /// 阶段五 C-2:每键**连续**失败次数(成功清零;未知错误走指数梯子)。
    failures: Mutex<HashMap<String, u32>>,
    default_cooldown_ms: i64,
}

impl CredentialPool {
    pub fn new() -> Self {
        Self::with_cooldown(DEFAULT_COOLDOWN_MS)
    }

    pub fn with_cooldown(default_cooldown_ms: i64) -> Self {
        Self {
            cooldowns: Mutex::new(HashMap::new()),
            counters: Mutex::new(HashMap::new()),
            last_outcomes: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            default_cooldown_ms,
        }
    }

    /// 是否处于冷却中;**已到期则顺手删除**(字典不无界增长)。
    pub fn is_cooled_down(&self, provider_id: &str, account_id: &str) -> bool {
        let key = state_key(provider_id, account_id);
        let mut guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = guard.get(&key) else {
            return false;
        };
        if entry.until_ms > wall_ms() {
            return true;
        }
        guard.remove(&key);
        false
    }

    /// 阶段五 C-2:(账号 + 模型)维度的冷却判定。
    ///
    /// 账号级冷却(key 失效/配额/余额/受限/待验证)对所有模型生效;
    /// 模型级冷却(限速/上游故障/未知错误)只影响该账号下的这一个模型
    /// —— 同一账号的其它模型仍可用。
    pub fn is_cooled_down_for_model(
        &self,
        provider_id: &str,
        account_id: &str,
        model: &str,
    ) -> bool {
        if self.is_cooled_down(provider_id, account_id) {
            return true;
        }
        if model.is_empty() {
            return false;
        }
        let key = model_state_key(provider_id, account_id, model);
        let mut guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = guard.get(&key) else {
            return false;
        };
        if entry.until_ms > wall_ms() {
            return true;
        }
        guard.remove(&key);
        false
    }

    /// (账号 + 模型)维度的冷却剩余毫秒:取账号级与模型级两者的较大值。
    pub fn cooldown_remaining_ms_for_model(
        &self,
        provider_id: &str,
        account_id: &str,
        model: &str,
    ) -> i64 {
        let account_left = self.cooldown_remaining_ms(provider_id, account_id);
        if model.is_empty() {
            return account_left;
        }
        let key = model_state_key(provider_id, account_id, model);
        let guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        let model_left = guard
            .get(&key)
            .map(|entry| (entry.until_ms - wall_ms()).max(0))
            .unwrap_or(0);
        account_left.max(model_left)
    }

    /// 阶段五 C-2:账号在**任一维度**上的冷却剩余毫秒(账号级 ∪ 模型级)。
    ///
    /// 管理面展示用:模型级冷却若不可见,用户会看到「明明在冷却却显示
    /// 可用」。模型级条目按前缀匹配取出最大剩余。
    pub fn account_cooling_remaining_ms(&self, provider_id: &str, account_id: &str) -> i64 {
        let account_left = self.cooldown_remaining_ms(provider_id, account_id);
        let prefix = format!("{provider_id}::{account_id}::");
        let guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        let now = wall_ms();
        let model_left = guard
            .iter()
            .filter(|(key, entry)| key.starts_with(&prefix) && entry.until_ms > now)
            .map(|(_, entry)| entry.until_ms - now)
            .max()
            .unwrap_or(0);
        account_left.max(model_left)
    }

    /// 冷却原因(人类可读);未在冷却中返回 `None`。
    pub fn cooldown_reason(&self, provider_id: &str, account_id: &str) -> Option<String> {
        let key = state_key(provider_id, account_id);
        let guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&key)
            .filter(|entry| entry.until_ms > wall_ms())
            .map(|entry| entry.reason.clone())
    }

    /// 应用一次上游结果:失败码设冷却,「可用」状态码清冷却。
    ///
    /// 任一 id 为空 → 直接返回(Python 同款短路)。
    pub fn record_outcome(&self, provider_id: &str, account_id: &str, status_code: i64) {
        self.record_outcome_with_retry_after(provider_id, account_id, status_code, None);
    }

    /// K3 扩展:带可选 `Retry-After`(毫秒)的结果记录。
    ///
    /// 兼容入口:没有模型维度、没有语义分类 → 按状态码取默认类别
    /// (401/403 → 鉴权、429 → 限速、5xx → 上游故障)。
    pub fn record_outcome_with_retry_after(
        &self,
        provider_id: &str,
        account_id: &str,
        status_code: i64,
        retry_after_ms: Option<i64>,
    ) {
        let signal = FailureSignal::new(default_rest_class(status_code), retry_after_ms);
        self.record_outcome_classified(provider_id, account_id, None, status_code, signal);
    }

    /// 阶段五 C-2:带模型维度与语义类别的结果记录(生产路径)。
    ///
    /// - 冷却时长按**语义类别**算(`wr_domain::rest`),不再只看状态码:
    ///   配额类用厂商 reset 时间(上限 8 天)、余额 30 分钟、受限 2 小时、
    ///   待验证 30 分钟、鉴权 300s、限速/上游故障 60s(可被 `Retry-After`
    ///   覆盖)、未知错误走 1m→10m 指数梯子;
    /// - 冷却**作用域**按类别分:账号级类别(key 失效/配额/余额/受限/
    ///   待验证)写账号键 —— 整把 key 停用;模型级类别(限速/上游故障/
    ///   未知)写 (账号+模型)键 —— 同一账号的其它模型仍可用;
    /// - 到期时刻加**确定性抖动**(按账号+模型派生):同批冷却的账号不在
    ///   同一毫秒集体恢复;
    /// - 无论结果如何都记录 `last_status_code`(K5 观测面);成功清空连续
    ///   失败计数(指数梯子复位)。
    pub fn record_outcome_classified(
        &self,
        provider_id: &str,
        account_id: &str,
        model: Option<&str>,
        status_code: i64,
        signal: FailureSignal,
    ) {
        if provider_id.is_empty() || account_id.is_empty() {
            return;
        }
        let key = state_key(provider_id, account_id);
        self.last_outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), LastOutcome { status_code });
        let mut guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        if should_cooldown(status_code) {
            let failures = self.bump_failures(&key);
            // 限速/上游故障在「厂商没给时长」时用池默认档(与既有语义
            // 一致);其余类别用语义基准/厂商 reset。
            let duration_ms = match (signal.class, signal.reset_ms) {
                (RestClass::RateLimit | RestClass::ServerError, None) => self.default_cooldown_ms,
                _ => rest_duration_ms(signal.class, signal.reset_ms, failures),
            };
            let cooldown = Cooldown {
                reason: format!("HTTP {status_code} ({})", signal.class.as_str()),
                until_ms: wall_ms() + duration_ms + self.jitter_for(&key, model, duration_ms),
            };
            self.store_cooldown(
                &mut guard,
                provider_id,
                account_id,
                model,
                signal.class.is_account_scoped(),
                cooldown,
            );
        } else if status_in_2xx_to_4xx(status_code) {
            guard.remove(&key);
            if let Some(model) = model.filter(|model| !model.is_empty()) {
                guard.remove(&model_state_key(provider_id, account_id, model));
            }
            self.failures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        }
    }

    /// 阶段五 C-2:记录一次**无 HTTP 状态码**的失败(流式中途断连、
    /// 连接错误):归入未知错误类别,走指数梯子(1m→10m),作用域为模型级。
    ///
    /// 与「上游明确报错」区分:这类失败不说明账号有问题,只说明这一条
    /// 路断了 —— 不该把整把 key 停掉。
    pub fn record_transport_failure(
        &self,
        provider_id: &str,
        account_id: &str,
        model: Option<&str>,
        reason: &str,
    ) {
        if provider_id.is_empty() || account_id.is_empty() {
            return;
        }
        let key = state_key(provider_id, account_id);
        let failures = self.bump_failures(&key);
        let duration_ms = wr_domain::rest::rest_ladder_ms(failures);
        let cooldown = Cooldown {
            reason: reason.to_owned(),
            until_ms: wall_ms() + duration_ms + self.jitter_for(&key, model, duration_ms),
        };
        let mut guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        self.store_cooldown(&mut guard, provider_id, account_id, model, false, cooldown);
    }

    /// 连续失败计数 +1(返回新值);成功时清零。
    fn bump_failures(&self, key: &str) -> u32 {
        let mut counter = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        let entry = counter.entry(key.to_owned()).or_insert(0);
        *entry = entry.saturating_add(1);
        *entry
    }

    /// 到期抖动(按账号+模型派生,确定性)。
    fn jitter_for(&self, account_key: &str, model: Option<&str>, duration_ms: i64) -> i64 {
        let seed = match model.filter(|model| !model.is_empty()) {
            Some(model) => format!("{account_key}::{model}"),
            None => account_key.to_owned(),
        };
        cooldown_jitter_ms(&seed, duration_ms)
    }

    /// 按作用域落冷却:账号级类别写账号键;模型级类别写 (账号+模型)键;
    /// 没有模型维度时退回账号级(拿不准时限制更严)。
    fn store_cooldown(
        &self,
        guard: &mut HashMap<String, Cooldown>,
        provider_id: &str,
        account_id: &str,
        model: Option<&str>,
        account_scoped: bool,
        cooldown: Cooldown,
    ) {
        let model = model.filter(|model| !model.is_empty());
        if account_scoped || model.is_none() {
            guard.insert(state_key(provider_id, account_id), cooldown);
        } else {
            guard.insert(
                model_state_key(provider_id, account_id, model.unwrap_or_default()),
                cooldown,
            );
        }
    }

    /// 冷却剩余毫秒(K5 观测面);未在冷却中(含已到期顺手删除)→ 0。
    pub fn cooldown_remaining_ms(&self, provider_id: &str, account_id: &str) -> i64 {
        let key = state_key(provider_id, account_id);
        let guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&key)
            .map(|entry| (entry.until_ms - wall_ms()).max(0))
            .unwrap_or(0)
    }

    /// 每键最近一次上游结果的状态码(K5 观测面);从未记录过 → `None`。
    pub fn last_status_code(&self, provider_id: &str, account_id: &str) -> Option<i64> {
        let key = state_key(provider_id, account_id);
        self.last_outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .map(|outcome| outcome.status_code)
    }

    /// 清除冷却(管理面 `clearCredentialCooldown`):账号级与该账号下的
    /// 模型级冷却一并清除,连续失败计数一并复位(否则下一次失败仍走
    /// 指数梯子高位)。
    pub fn clear_cooldown(&self, provider_id: &str, account_id: &str) {
        let key = state_key(provider_id, account_id);
        let prefix = format!("{key}::");
        let mut guard = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&key);
        guard.retain(|entry_key, _| !entry_key.starts_with(&prefix));
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }

    /// 固定窗口计数:窗口起点 `(now / window) * window`,窗口换了就换桶。
    /// 返回累加后的新值;`window_ms <= 0` 或 id 为空 → 0(Python 同款)。
    pub fn increment_counter(
        &self,
        provider_id: &str,
        account_id: &str,
        rule_name: &str,
        window_ms: i64,
        amount: i64,
    ) -> i64 {
        if provider_id.is_empty() || account_id.is_empty() || window_ms <= 0 {
            return 0;
        }
        let window_start = (wall_ms() / window_ms) * window_ms;
        let key = counter_key(provider_id, account_id, rule_name, window_start);
        let mut guard = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let bucket = guard.entry(key).or_insert(CounterBucket {
            window_start_ms: window_start,
            value: 0,
        });
        bucket.value += amount;
        bucket.value
    }

    /// 当前窗口计数(桶不存在或已滚出 → 0)。
    pub fn counter_value(
        &self,
        provider_id: &str,
        account_id: &str,
        rule_name: &str,
        window_ms: i64,
    ) -> i64 {
        if provider_id.is_empty() || account_id.is_empty() || window_ms <= 0 {
            return 0;
        }
        let window_start = (wall_ms() / window_ms) * window_ms;
        let key = counter_key(provider_id, account_id, rule_name, window_start);
        let guard = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&key)
            .filter(|bucket| bucket.window_start_ms == window_start)
            .map_or(0, |bucket| bucket.value)
    }
}

/// weighted/sticky 加权随机用的轻量 PRNG(splitmix64),避免为此引入
/// `rand` 依赖。`from_entropy` 以 wall_ms + 进程内原子计数器混淆做种子,
/// 同毫秒并发请求也不会得到相同序列;单测直接注入 `&mut impl FnMut() ->
/// f64` 固定序列保证确定性。
pub struct WeightedRng {
    state: u64,
}

impl WeightedRng {
    pub fn from_entropy() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let seed = (wall_ms() as u64).rotate_left(17) ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        Self { state: seed }
    }

    /// 产出 [0, 1) 的均匀随机数(53 位精度)。
    pub fn next_f64(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// K2 策略化选号(方案 20260905_02 §3.2):按 provider 的
/// `credential_strategy` / `pinned_account_id` / 账号权重选号。
///
/// 语义(与方案伪代码逐条对应):
/// - 候选 = `enabled`(保持配置顺序);可用 = 候选剔除冷却中;
/// - `first`:`(priority DESC, 配置顺序)` 取可用第一个;
/// - `pinned`:严格固定 `pinned_account_id` —— enabled 才返回,**无视
///   冷却也不回落**(账号 disabled/不存在/未配置 → `None`,上层走
///   legacy/报错路径,不偷换);
/// - `weighted`:`weight.max(1)` 累计减权随机抽取(可用集合);
/// - `balanced`(UI「负载均衡」):可用集合中取最高优先级层,层内 weighted;
/// - `sticky`:有会话键 → 加权 rendezvous(Efraimidis-Spirakis,
///   score = u^(1/w),u 哈希自 `session_key || account_id`,取最大);
///   无会话键 → weighted 行为;
/// - 可用为空 → fail-open 回落候选第一个 enabled(保留现状);
/// - 候选为空 → `None`。
///
/// `rng` 供 weighted/sticky(无会话键回落)使用;生产调用传
/// [`WeightedRng`],单测注入固定闭包保证确定性。
pub fn pick_account_with(
    rng: &mut impl FnMut() -> f64,
    pool: &CredentialPool,
    provider: &Provider,
    session_key: Option<&str>,
) -> Option<usize> {
    pick_account_with_model(rng, pool, provider, session_key, "")
}

/// 阶段五 C-2:带模型维度的策略化选号(生产路径)。
///
/// 与 [`pick_account_with`] 的语义完全一致,只是冷却判定多一个模型
/// 维度(账号级冷却对所有模型生效,模型级冷却只剔除该模型);
/// `model` 为空串时等价于无模型维度(只按账号级冷却剔除)。
pub fn pick_account_with_model(
    rng: &mut impl FnMut() -> f64,
    pool: &CredentialPool,
    provider: &Provider,
    session_key: Option<&str>,
    model: &str,
) -> Option<usize> {
    let accounts = &provider.accounts[..];
    let provider_id = provider.id.as_str();
    let candidates: Vec<usize> = accounts
        .iter()
        .enumerate()
        .filter(|(_, account)| account.enabled)
        .map(|(index, _)| index)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    if provider.credential_strategy == ProviderCredentialStrategy::Pinned {
        // 严格固定:不看冷却、不回落;disabled(不在候选里)/不存在/
        // 未配置都返回 None。
        let pinned = provider.pinned_account_id.as_deref()?;
        return candidates.into_iter().find(|&i| accounts[i].id == pinned);
    }
    let usable: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|&i| !pool.is_cooled_down_for_model(provider_id, &accounts[i].id, model))
        .collect();
    if usable.is_empty() {
        // 阶段五 C-2 probeOne:全冷却时不再盲目打第一个,而是探**最早
// 恢复**的那个账号 ——
        // 「宁打一把快恢复的冷却 key」好过「打一把还要等两小时的」。
        return candidates
            .iter()
            .copied()
            .min_by_key(|&i| {
                pool.cooldown_remaining_ms_for_model(provider_id, &accounts[i].id, model)
            })
            .or_else(|| candidates.first().copied());
    }
    match provider.credential_strategy {
        ProviderCredentialStrategy::First => pick_first_usable(accounts, &usable),
        ProviderCredentialStrategy::Weighted => weighted_pick(rng, accounts, &usable),
        ProviderCredentialStrategy::Balanced => balanced_pick(rng, accounts, &usable),
        ProviderCredentialStrategy::Sticky => match session_key {
            Some(key) => Some(sticky_pick(key, accounts, &usable)),
            None => weighted_pick(rng, accounts, &usable),
        },
        ProviderCredentialStrategy::Pinned => unreachable!("pinned 分支已提前返回"),
    }
}

/// balanced 策略(2026-09-06 UI 两模式的「负载均衡」):先取可用集合中
/// 优先级最高的层级(主备语义,同 first),层内按 `weight.max(1)` 加权
/// 随机分摊(同 weighted)——两机制的结合。整层冷却时自然落到下一层。
fn balanced_pick(
    rng: &mut impl FnMut() -> f64,
    accounts: &[ProviderAccount],
    usable: &[usize],
) -> Option<usize> {
    let top_priority = usable
        .iter()
        .map(|&index| accounts[index].priority)
        .max()?;
    let tier: Vec<usize> = usable
        .iter()
        .copied()
        .filter(|&index| accounts[index].priority == top_priority)
        .collect();
    weighted_pick(rng, accounts, &tier)
}

/// 可用集保持配置顺序,遍历取优先级最大者(同优先级先见者胜)。
fn pick_first_usable(accounts: &[ProviderAccount], usable: &[usize]) -> Option<usize> {
    let mut best = *usable.first()?;
    for &index in &usable[1..] {
        if accounts[index].priority > accounts[best].priority {
            best = index;
        }
    }
    Some(best)
}

/// weighted 策略:`weight.max(1)` 累计减权抽取(无状态加权随机)。
///
fn weighted_pick(
    rng: &mut impl FnMut() -> f64,
    accounts: &[ProviderAccount],
    usable: &[usize],
) -> Option<usize> {
    let total: f64 = usable
        .iter()
        .map(|&i| accounts[i].weight.max(1) as f64)
        .sum();
    let mut rest = rng() * total;
    for &index in usable {
        rest -= accounts[index].weight.max(1) as f64;
        if rest < 0.0 {
            return Some(index);
        }
    }
    // 浮点误差兜底:落到最后一个可用项。
    usable.last().copied()
}

/// sticky 策略:加权 rendezvous(Efraimidis-Spirakis 单次抽取),
/// score = u^(1/w),u = `sticky_score_u(session_key, account_id)` ∈ [0,1);
/// 取 score 最大者(同分先见者胜,配置顺序稳定)。
fn sticky_pick(session_key: &str, accounts: &[ProviderAccount], usable: &[usize]) -> usize {
    let mut best = usable[0];
    let mut best_score = f64::MIN;
    for &index in usable {
        let u = wr_domain::gateway::sticky_score_u(session_key, &accounts[index].id);
        let score = u.powf(1.0 / accounts[index].weight.max(1) as f64);
        if score > best_score {
            best_score = score;
            best = index;
        }
    }
    best
}

/// 用账号密钥替换 provider 凭据(镜像 `select_provider` 的账号注入步)。
///
/// 只在池里真的取到账号 **且** 该账号在 `secrets` 里存有非空密钥时替换;
/// 否则原样返回 —— 与 Python 的 `if stored:` 一致(空密钥不得把已注入的
/// legacy 单密钥冲掉)。
pub fn with_account_credential(
    mut provider: wr_domain::provider::Provider,
    account: Option<&wr_domain::provider::ProviderAccount>,
    stored_secret: &str,
) -> wr_domain::provider::Provider {
    if account.is_some() && !stored_secret.is_empty() {
        provider.api_key = stored_secret.to_owned();
    }
    provider
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    // ---- 20260905_02 K2:策略化选号矩阵 -----------------------------------
    // 四策略 × (多 key / 全冷却 / 单 key / 空池 / pinned 账号禁用)。

    use serde_json::{Value, json};
    use wr_domain::provider::Provider;
    use wr_domain::provider::ProviderCredentialStrategy as Strategy;

    /// 经 `Provider::from_raw` 构造(顺带钉住 accounts 的配置解析路径)。
    fn provider_with(strategy: Strategy, pinned: Option<&str>, accounts: Value) -> Provider {
        let mut raw = serde_json::Map::new();
        raw.insert("id".into(), json!("prov"));
        raw.insert("api_base_url".into(), json!("http://127.0.0.1:1/v1"));
        raw.insert("credentialStrategy".into(), json!(strategy.as_str()));
        if let Some(pinned) = pinned {
            raw.insert("pinnedAccountId".into(), json!(pinned));
        }
        raw.insert("accounts".into(), accounts);
        Provider::from_raw(&raw).unwrap()
    }

    fn weighted_accounts(weights: &[(&str, i64)], priority: i64) -> Value {
        json!(
            weights
                .iter()
                .map(|(id, weight)| json!({
                    "id": id, "enabled": true, "weight": weight, "priority": priority,
                }))
                .collect::<Vec<_>>()
        )
    }

    /// 确定性伪随机源(固定种子的 LCG),供分布断言复现。
    fn lcg(seed: u64) -> impl FnMut() -> f64 {
        let mut state = seed;
        move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[test]
    fn first_strategy_orders_by_priority_then_config() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::First,
            None,
            json!([
                {"id": "a1", "enabled": true, "priority": 0},
                {"id": "a2", "enabled": true, "priority": 5},
                {"id": "a3", "enabled": true, "priority": 5},
            ]),
        );
        // priority DESC:a2/a3(5)先于 a1(0);同优先级配置顺序 a2 胜。
        assert_eq!(
            pick_account_with(&mut lcg(1), &pool, &provider, None),
            Some(1)
        );
        // a2 冷却 → 同档 a3;a3 也冷却 → a1。
        pool.record_outcome("prov", "a2", 429);
        assert_eq!(
            pick_account_with(&mut lcg(1), &pool, &provider, None),
            Some(2)
        );
        pool.record_outcome("prov", "a3", 429);
        assert_eq!(
            pick_account_with(&mut lcg(1), &pool, &provider, None),
            Some(0)
        );
    }

    #[test]
    fn pinned_strategy_is_strict_no_swap_no_fallback() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::Pinned,
            Some("a2"),
            weighted_accounts(&[("a1", 1), ("a2", 1), ("a3", 1)], 0),
        );
        assert_eq!(
            pick_account_with(&mut lcg(1), &pool, &provider, None),
            Some(1)
        );
        // pinned 账号冷却:**无视冷却也不偷换**(用户意志最高)。
        pool.record_outcome("prov", "a2", 503);
        assert!(pool.is_cooled_down("prov", "a2"));
        assert_eq!(
            pick_account_with(&mut lcg(1), &pool, &provider, None),
            Some(1)
        );
    }

    #[test]
    fn pinned_strategy_disabled_or_missing_account_yields_none() {
        let pool = CredentialPool::new();
        // pinned 指向 disabled 账号:返回 None(上层走 legacy/报错),不回落。
        let disabled = provider_with(
            Strategy::Pinned,
            Some("a2"),
            json!([
                {"id": "a1", "enabled": true},
                {"id": "a2", "enabled": false},
            ]),
        );
        assert_eq!(pick_account_with(&mut lcg(1), &pool, &disabled, None), None);
        // pinned 指向不存在的账号。
        let missing = provider_with(
            Strategy::Pinned,
            Some("ghost"),
            weighted_accounts(&[("a1", 1)], 0),
        );
        assert_eq!(pick_account_with(&mut lcg(1), &pool, &missing, None), None);
        // 未配置 pinned_account_id。
        let unconfigured =
            provider_with(Strategy::Pinned, None, weighted_accounts(&[("a1", 1)], 0));
        assert_eq!(
            pick_account_with(&mut lcg(1), &pool, &unconfigured, None),
            None
        );
    }

    #[test]
    fn weighted_strategy_draws_by_weight() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::Weighted,
            None,
            weighted_accounts(&[("heavy", 9), ("light", 1)], 0),
        );
        // rng()=0 → 落在第一段(heavy);rng()=0.999 → 落在末段(light)。
        assert_eq!(
            pick_account_with(&mut || 0.0_f64, &pool, &provider, None),
            Some(0)
        );
        assert_eq!(
            pick_account_with(&mut || 0.999_f64, &pool, &provider, None),
            Some(1)
        );
        // 固定种子分布:1000 次抽样,heavy ≈ 90%(权重 9:1)。
        let mut rng = lcg(42);
        let mut heavy = 0;
        for _ in 0..1000 {
            if pick_account_with(&mut rng, &pool, &provider, None) == Some(0) {
                heavy += 1;
            }
        }
        assert!(
            (850..950).contains(&heavy),
            "权重 9:1 下 heavy 应约 90%,实际 {heavy}/1000"
        );
    }

    #[test]
    fn weighted_strategy_excludes_cooled_and_falls_back_fail_open() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::Weighted,
            None,
            weighted_accounts(&[("a1", 5), ("a2", 1)], 0),
        );
        // a1 冷却被剔除 → 只剩 a2,rng 值不影响结果。
        pool.record_outcome("prov", "a1", 429);
        assert_eq!(
            pick_account_with(&mut || 0.0_f64, &pool, &provider, None),
            Some(1)
        );
// 全冷却 → probeOne 回落:探**最早恢复**的那个账号,
        // 不再是配置顺序第一个。
        pool.record_outcome("prov", "a2", 429);
        let picked = pick_account_with(&mut || 0.999_f64, &pool, &provider, None).unwrap();
        let picked_left = pool.cooldown_remaining_ms("prov", &provider.accounts[picked].id);
        let other_left = pool.cooldown_remaining_ms("prov", &provider.accounts[1 - picked].id);
        assert!(
            picked_left <= other_left,
            "probeOne 必须探最早恢复的账号(选中 {picked},剩余 {picked_left} vs {other_left})"
        );
    }

    #[test]
    fn weighted_strategy_single_key_and_empty_pool() {
        let pool = CredentialPool::new();
        let single = provider_with(
            Strategy::Weighted,
            None,
            weighted_accounts(&[("only", 3)], 0),
        );
        assert_eq!(
            pick_account_with(&mut || 0.7_f64, &pool, &single, None),
            Some(0)
        );
        let empty = provider_with(Strategy::Weighted, None, json!([]));
        assert_eq!(
            pick_account_with(&mut || 0.7_f64, &pool, &empty, None),
            None
        );
        // 全 disabled 同样是空候选。
        let all_disabled = provider_with(
            Strategy::Weighted,
            None,
            json!([{"id": "a1", "enabled": false}]),
        );
        assert_eq!(
            pick_account_with(&mut || 0.7_f64, &pool, &all_disabled, None),
            None
        );
    }

    #[test]
    fn sticky_strategy_is_stable_per_session() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::Sticky,
            None,
            weighted_accounts(&[("a1", 1), ("a2", 1), ("a3", 1)], 0),
        );
        // 同一会话键重复选号 → 同一把 key(粘性)。
        let first = pick_account_with(&mut lcg(7), &pool, &provider, Some("session-a"));
        assert_eq!(
            first,
            pick_account_with(&mut lcg(8), &pool, &provider, Some("session-a")),
            "同会话必须稳定"
        );
        assert!(first.is_some());
        // 权重进评分:大权重账号在多数会话键下胜出(53 位哈希下的强断言:
        // w=1000 的 score=u^(1/1000)≈1 恒压过 w=1 的 score=u)。
        let skewed = provider_with(
            Strategy::Sticky,
            None,
            weighted_accounts(&[("big", 1000), ("small", 1)], 0),
        );
        for session in ["s1", "s2", "s3", "s4", "s5"] {
            assert_eq!(
                pick_account_with(&mut lcg(9), &pool, &skewed, Some(session)),
                Some(0),
                "session={session}:大权重应吸引绝大多数会话"
            );
        }
    }

    #[test]
    fn sticky_strategy_without_session_key_behaves_weighted() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::Sticky,
            None,
            weighted_accounts(&[("a1", 5), ("a2", 1)], 0),
        );
        // 无会话键 → weighted 行为:rng()=0 → 第一段;rng()=0.999 → 末段。
        assert_eq!(
            pick_account_with(&mut || 0.0_f64, &pool, &provider, None),
            Some(0)
        );
        assert_eq!(
            pick_account_with(&mut || 0.999_f64, &pool, &provider, None),
            Some(1)
        );
    }

    #[test]
    fn sticky_strategy_excludes_cooled_and_falls_back_fail_open() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::Sticky,
            None,
            weighted_accounts(&[("a1", 1), ("a2", 1)], 0),
        );
        // a1 冷却被剔除:即使哈希评分偏爱 a1,也只能选 a2。
        let pinned_by_hash = pick_account_with(&mut lcg(3), &pool, &provider, Some("s"));
        pool.record_outcome("prov", "a1", 429);
        assert_eq!(
            pick_account_with(&mut lcg(3), &pool, &provider, Some("s")),
            Some(1),
            "冷却中的 key 必须剔除(此前选中 {pinned_by_hash:?})"
        );
        // 全冷却 → probeOne 回落:探最早恢复的那个账号。
        pool.record_outcome("prov", "a2", 429);
        let picked = pick_account_with(&mut lcg(3), &pool, &provider, Some("s")).unwrap();
        let picked_left = pool.cooldown_remaining_ms("prov", &provider.accounts[picked].id);
        let other_left = pool.cooldown_remaining_ms("prov", &provider.accounts[1 - picked].id);
        assert!(
            picked_left <= other_left,
            "probeOne 必须探最早恢复的账号(选中 {picked},剩余 {picked_left} vs {other_left})"
        );
    }

    // ---- 阶段五 C-2:probeOne 与模型维度选号 -------------------------------

/// 全冷却时探**最早恢复**的那个账号(probeOne)。
    #[test]
    fn probe_one_picks_the_earliest_recovering_account() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::First,
            None,
            weighted_accounts(&[("a1", 1), ("a2", 1)], 0),
        );
        // a1 鉴权失败(300s 账号级),a2 限速(60s 模型级)→ 应探 a2。
        pool.record_outcome_classified(
            "prov",
            "a1",
            Some("m"),
            401,
            FailureSignal::new(RestClass::Auth, None),
        );
        pool.record_outcome_classified(
            "prov",
            "a2",
            Some("m"),
            429,
            FailureSignal::new(RestClass::RateLimit, None),
        );
        let picked = pick_account_with_model(&mut || 0.0_f64, &pool, &provider, None, "m").unwrap();
        assert_eq!(picked, 1, "应探最早恢复的 a2");
    }

    /// 模型级冷却只剔除该模型的候选:同一账号的其它模型仍可用。
    #[test]
    fn model_scoped_cooldown_keeps_other_models_usable() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::First,
            None,
            weighted_accounts(&[("a1", 1), ("a2", 1)], 0),
        );
        // a1 在模型 m1 上被限速。
        pool.record_outcome_classified(
            "prov",
            "a1",
            Some("m1"),
            429,
            FailureSignal::new(RestClass::RateLimit, None),
        );
        // 模型 m2:a1 可用 → 选 a1(配置顺序第一个可用)。
        let picked =
            pick_account_with_model(&mut || 0.0_f64, &pool, &provider, None, "m2").unwrap();
        assert_eq!(picked, 0, "别的模型不该被限速牵连");
        // 模型 m1:a1 被剔除 → 选 a2。
        let picked =
            pick_account_with_model(&mut || 0.0_f64, &pool, &provider, None, "m1").unwrap();
        assert_eq!(picked, 1, "被限速的模型应跳到下一个账号");
    }

    // ---- 2026-09-06 UI 两模式:balanced = 优先级分层 × 层内加权 ------------

    #[test]
    fn balanced_pick_respects_priority_tiers_then_weight() {
        let pool = CredentialPool::new();
        // 主层(a1 优先级 5)与备层(a2/a3 优先级 0):永远先在主层内抽。
        let provider = provider_with(
            Strategy::Balanced,
            None,
            json!([
                {"id": "a1", "enabled": true, "priority": 5, "weight": 1},
                {"id": "a2", "enabled": true, "priority": 0, "weight": 9},
                {"id": "a3", "enabled": true, "priority": 0, "weight": 1},
            ]),
        );
        // 备层权重再大也不越过主层。
        for seed in [1u64, 7, 42, 99, 123] {
            assert_eq!(
                pick_account_with(&mut lcg(seed), &pool, &provider, None),
                Some(0),
                "seed {seed}: 层级优先于权重"
            );
        }
        // 主层冷却 → 整层落到备层,层内按权重抽取。
        pool.record_outcome("prov", "a1", 429);
        assert_eq!(
            pick_account_with(&mut || 0.0_f64, &pool, &provider, None),
            Some(1),
            "备层内 weight=9 的 a2 抽中低段"
        );
        assert_eq!(
            pick_account_with(&mut || 0.999_f64, &pool, &provider, None),
            Some(2),
            "备层内 weight=1 的 a3 抽中高段"
        );
    }

    #[test]
    fn balanced_pick_within_top_tier_distributes_by_weight() {
        let pool = CredentialPool::new();
        let provider = provider_with(
            Strategy::Balanced,
            None,
            weighted_accounts(&[("heavy", 9), ("light", 1)], 3),
        );
        // 同层:与 weighted 同一套累计减权抽取。
        assert_eq!(pick_account_with(&mut || 0.0_f64, &pool, &provider, None), Some(0));
        assert_eq!(
            pick_account_with(&mut || 0.999_f64, &pool, &provider, None),
            Some(1)
        );
    }

    #[test]
    fn balanced_parse_and_default_roundtrip() {
        assert_eq!(Strategy::parse("balanced"), Strategy::Balanced);
        assert_eq!(Strategy::Balanced.as_str(), "balanced");
        // 非法值仍宽容回退 first。
        assert_eq!(Strategy::parse("nope"), Strategy::First);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_shapes_match_python() {
        assert_eq!(state_key("p", "a"), "p::a");
        assert_eq!(counter_key("p", "a", "rps", 1000), "p::a::rps::1000");
    }

    #[test]
    fn cooldown_status_rules() {
        for code in [401, 403, 429, 500, 502, 503] {
            assert!(should_cooldown(code), "{code} 应触发冷却");
        }
        assert!(!should_cooldown(400), "400 是客户端错误,不冷却凭据");
        assert!(!should_cooldown(200));
        // status_in_2xx_to_4xx:200..500 且不在冷却码里。
        assert!(status_in_2xx_to_4xx(200));
        assert!(status_in_2xx_to_4xx(404));
        assert!(!status_in_2xx_to_4xx(429));
        assert!(!status_in_2xx_to_4xx(500));
    }

    // ---- 20260905_02 K3:分档冷却 ----------------------------------------

    #[test]
    fn cooldown_tier_maps_status_codes() {
        // 401/403 → 300s(凭据级失效);429/5xx → 60s;其余 None。
        assert_eq!(cooldown_tier(401), Some(CREDENTIAL_COOLDOWN_MS));
        assert_eq!(cooldown_tier(403), Some(CREDENTIAL_COOLDOWN_MS));
        assert_eq!(cooldown_tier(429), Some(DEFAULT_COOLDOWN_MS));
        for code in [500, 502, 503, 599] {
            assert_eq!(cooldown_tier(code), Some(DEFAULT_COOLDOWN_MS), "{code}");
        }
        for code in [200, 400, 404, 422] {
            assert_eq!(cooldown_tier(code), None, "{code} 不冷却");
        }
    }

    #[test]
    fn auth_failures_cool_longer_than_rate_limits() {
        let pool = CredentialPool::new();
        pool.record_outcome("p", "a401", 401);
        pool.record_outcome("p", "a429", 429);
        let auth_left = pool.cooldown_remaining_ms("p", "a401");
        let rate_left = pool.cooldown_remaining_ms("p", "a429");
        assert!(
            auth_left > 250_000,
            "401 应进 300s 档,实际剩余 {auth_left}ms"
        );
        // 阶段五 C-2:到期时刻带抖动 → 上限放宽一个抖动幅度。
        assert!(
            auth_left <= CREDENTIAL_COOLDOWN_MS + MAX_COOLDOWN_JITTER_MS,
            "冷却不得超过档位上限(+抖动),实际 {auth_left}ms"
        );
        assert!(
            rate_left > 0 && rate_left <= DEFAULT_COOLDOWN_MS + MAX_COOLDOWN_JITTER_MS,
            "429 应进 60s 档(+抖动),实际剩余 {rate_left}ms"
        );
    }

    #[test]
    fn retry_after_overrides_rate_limit_tier_only() {
        let pool = CredentialPool::new();
        // 429 带 Retry-After=120s:取上游值(>默认 60s)。
        pool.record_outcome_with_retry_after("p", "a429", 429, Some(120_000));
        let left = pool.cooldown_remaining_ms("p", "a429");
        assert!(
            left > 60_000 && left <= 120_000 + MAX_COOLDOWN_JITTER_MS,
            "429 应采用 Retry-After(+抖动),实际 {left}ms"
        );
        // 5xx 同样消费 Retry-After。
        pool.record_outcome_with_retry_after("p", "a500", 500, Some(90_000));
        let left = pool.cooldown_remaining_ms("p", "a500");
        assert!(
            left > 60_000 && left <= 90_000 + MAX_COOLDOWN_JITTER_MS,
            "500 应采用 Retry-After(+抖动),实际 {left}ms"
        );
        // 401 忽略 Retry-After(固定 300s 档)。
        pool.record_outcome_with_retry_after("p", "a401", 401, Some(1_000));
        let left = pool.cooldown_remaining_ms("p", "a401");
        assert!(left > 250_000, "401 不吃 Retry-After,实际 {left}ms");
        // 非 429/5xx 不冷却,Retry-After 无意义。
        pool.record_outcome_with_retry_after("p", "a400", 400, Some(1_000));
        assert!(!pool.is_cooled_down("p", "a400"));
    }

    #[test]
    fn retry_after_is_capped_at_24h_and_ignores_non_positive() {
        let pool = CredentialPool::new();
        // 恶意 Retry-After 锁死防护:cap 24h。
        pool.record_outcome_with_retry_after("p", "a429", 429, Some(86_400_000 * 10));
        let left = pool.cooldown_remaining_ms("p", "a429");
        assert!(
            left <= MAX_RETRY_AFTER_COOLDOWN_MS + MAX_COOLDOWN_JITTER_MS,
            "Retry-After 不得超过 24h(+抖动),实际 {left}ms"
        );
        assert!(left > 86_000_000, "应取 cap 而非默认 60s");
        // 非正值 → 回落默认档。
        pool.record_outcome_with_retry_after("p", "a429b", 429, Some(0));
        let left = pool.cooldown_remaining_ms("p", "a429b");
        assert!(left > 0 && left <= DEFAULT_COOLDOWN_MS + MAX_COOLDOWN_JITTER_MS);
        pool.record_outcome_with_retry_after("p", "a429c", 429, Some(-5));
        assert!(
            pool.cooldown_remaining_ms("p", "a429c")
                <= DEFAULT_COOLDOWN_MS + MAX_COOLDOWN_JITTER_MS
        );
    }

    #[test]
    fn last_status_code_is_recorded_for_stats() {
        let pool = CredentialPool::new();
        assert_eq!(pool.last_status_code("p", "a"), None);
        pool.record_outcome("p", "a", 429);
        assert_eq!(pool.last_status_code("p", "a"), Some(429));
        // 2xx 清冷却,但最近状态码作为观测面保留。
        pool.record_outcome("p", "a", 200);
        assert!(!pool.is_cooled_down("p", "a"));
        assert_eq!(pool.last_status_code("p", "a"), Some(200));
    }

    #[test]
    fn record_outcome_sets_and_clears_cooldown() {
        let pool = CredentialPool::with_cooldown(60_000);
        pool.record_outcome("p", "a", 429);
        assert!(pool.is_cooled_down("p", "a"));
        // 阶段五 C-2:冷却原因带上语义类别(429 → 限速)。
        assert_eq!(
            pool.cooldown_reason("p", "a").as_deref(),
            Some("HTTP 429 (rate_limit)")
        );
        // 成功(2xx)清冷却:「用过就算数」。
        pool.record_outcome("p", "a", 200);
        assert!(!pool.is_cooled_down("p", "a"));
        assert!(pool.cooldown_reason("p", "a").is_none());
    }

    #[test]
    fn empty_ids_are_noops() {
        let pool = CredentialPool::new();
        pool.record_outcome("", "a", 429);
        pool.record_outcome("p", "", 429);
        assert!(!pool.is_cooled_down("", ""));
        assert_eq!(pool.increment_counter("p", "", "r", 1000, 1), 0);
        assert_eq!(pool.increment_counter("p", "a", "r", 0, 1), 0);
        assert_eq!(pool.counter_value("p", "a", "r", -5), 0);
    }

    #[test]
    fn counters_are_windowed_per_rule() {
        let pool = CredentialPool::new();
        // 1 小时窗口:整个测试进程生命周期都在同一桶里。
        assert_eq!(pool.increment_counter("p", "a", "rph", 3_600_000, 1), 1);
        assert_eq!(pool.increment_counter("p", "a", "rph", 3_600_000, 2), 3);
        assert_eq!(pool.counter_value("p", "a", "rph", 3_600_000), 3);
        assert_eq!(
            pool.counter_value("p", "a", "other", 3_600_000),
            0,
            "规则名隔离"
        );
        assert_eq!(
            pool.counter_value("p", "b", "rph", 3_600_000),
            0,
            "账号隔离"
        );
    }

    #[test]
    fn clear_cooldown_forces_release() {
        let pool = CredentialPool::new();
        pool.record_outcome("p", "a", 500);
        assert!(pool.is_cooled_down("p", "a"));
        pool.clear_cooldown("p", "a");
        assert!(!pool.is_cooled_down("p", "a"));
    }

    // ---- 阶段五 C-2:语义分类 / 抖动 / 双计数 / 作用域 ---------------------

    #[test]
    fn cooldown_jitter_is_deterministic_and_bounded() {
        // 同种子恒等(可测、进程间一致)。
        assert_eq!(
            cooldown_jitter_ms("p::a::m", 60_000),
            cooldown_jitter_ms("p::a::m", 60_000)
        );
        // 幅度 = min(时长/5, 30s)。
        for seed in ["a", "b", "c", "d", "e"] {
            let jitter = cooldown_jitter_ms(seed, 60_000);
            assert!(jitter >= 0 && jitter <= 12_000, "{seed}: {jitter}");
            let jitter = cooldown_jitter_ms(seed, 600_000);
            assert!(jitter <= MAX_COOLDOWN_JITTER_MS, "{seed}: {jitter}");
        }
        // 非正时长 → 无抖动。
        assert_eq!(cooldown_jitter_ms("x", 0), 0);
    }

    #[test]
    fn quota_failure_cools_account_wide() {
        let pool = CredentialPool::new();
        pool.record_outcome_classified(
            "p",
            "a",
            Some("m1"),
            429,
            FailureSignal::new(RestClass::Quota, Some(3_600_000)),
        );
        // 账号级作用域 → 任意模型都算冷却(整把 key 停用)。
        assert!(pool.is_cooled_down("p", "a"));
        assert!(pool.is_cooled_down_for_model("p", "a", "m1"));
        assert!(pool.is_cooled_down_for_model("p", "a", "m2"));
        // 时长按语义类别(厂商 reset 1 小时),不再是一律 60s。
        let left = pool.cooldown_remaining_ms("p", "a");
        assert!(left > 3_000_000, "配额类应等小时级,实际 {left}ms");
    }

    #[test]
    fn rate_limit_failure_cools_model_scope_only() {
        let pool = CredentialPool::new();
        pool.record_outcome_classified(
            "p",
            "a",
            Some("m1"),
            429,
            FailureSignal::new(RestClass::RateLimit, None),
        );
        // 模型级作用域 → 账号级看不到,别的模型不受影响。
        assert!(!pool.is_cooled_down("p", "a"));
        assert!(pool.is_cooled_down_for_model("p", "a", "m1"));
        assert!(!pool.is_cooled_down_for_model("p", "a", "m2"));
        // 管理面口径:任一维度冷却都要看得见。
        assert!(pool.account_cooling_remaining_ms("p", "a") > 0);
    }

    #[test]
    fn transport_failure_uses_ladder_and_model_scope() {
        let pool = CredentialPool::new();
        pool.record_transport_failure("p", "a", Some("m1"), "upstream stream interrupted");
        assert!(pool.is_cooled_down_for_model("p", "a", "m1"));
        assert!(!pool.is_cooled_down_for_model("p", "a", "m2"));
        let first = pool.cooldown_remaining_ms_for_model("p", "a", "m1");
        // 连续失败 → 指数梯子翻倍。
        pool.record_transport_failure("p", "a", Some("m1"), "upstream stream interrupted");
        let second = pool.cooldown_remaining_ms_for_model("p", "a", "m1");
        assert!(second > first, "第二次失败应更长(梯子),{first} → {second}");
        // 成功复位失败计数并清冷却。
        pool.record_outcome_classified("p", "a", Some("m1"), 200, FailureSignal::default());
        assert!(!pool.is_cooled_down_for_model("p", "a", "m1"));
    }

    #[test]
    fn clear_cooldown_removes_model_scope_entries() {
        let pool = CredentialPool::new();
        pool.record_outcome_classified(
            "p",
            "a",
            Some("m1"),
            429,
            FailureSignal::new(RestClass::RateLimit, None),
        );
        assert!(pool.is_cooled_down_for_model("p", "a", "m1"));
        pool.clear_cooldown("p", "a");
        assert!(!pool.is_cooled_down_for_model("p", "a", "m1"));
        assert_eq!(pool.account_cooling_remaining_ms("p", "a"), 0);
    }
}
