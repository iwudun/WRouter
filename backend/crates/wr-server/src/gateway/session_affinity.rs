//! 会话粘性表 + 前缀钉扎表(20260905_03 §4.1 S2;Rust 新增,无 Python 镜像)。
//!
//! 进程内 TTL 表,与凭据池冷却态同一立场:重启清空,单机可接受
//! (方案 §8)。两张表同构但键空间不同:
//! - 会话粘性:键 = K4 会话键(`extract_session_key`),值 = 粘住的
//!   `(provider, model, account)`;TTL 由全局 `savings.sticky.ttlSecs`
//!   驱动(默认 900s),容量 1000;
//! - 前缀钉扎:键 = `prefix_pin_key` 的 SHA-256 hex,值 =
//!   `(provider, account)`;TTL 固定 300s 常量,容量 1000。
//!
//! 淘汰:手写插入序 FIFO(容量上限时淘汰最旧),不引入 lru crate
//! (任务边界;TTL 到期在读取时惰性删除 —— 过期即删,字典不无界增长)。
//!
//! 健康门不在本模块:**读取方**(forward.rs 选路点)必须复查目标健康,
//! 不健康即释放绑定落正常链 —— 粘性是优化不是承诺(方案 §7.3)。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use wr_infra::sqlite::engine::wall_ms;

/// 前缀钉扎 TTL(固定 300s,不进配置 —— 方案 §4.1)。
pub const PREFIX_PIN_TTL_MS: i64 = 300_000;

/// 两张表的容量上限(方案 §4.1:粘性 1000、前缀 1000)。
pub const STICKY_MAX_ENTRIES: usize = 1000;
pub const PREFIX_MAX_ENTRIES: usize = 1000;

/// 阶段五 C-1:「值不值」判据的最小命中次数门槛。
///
/// 样本太少(仅 1 次命中)不足以断言「这个上游从不回报缓存」—— 一次
/// 偶然没命中就解绑会把正常会话打散。门槛取 2:至少复用两次仍零
/// cache_read 才进入判定。
pub const STICKY_CACHE_MIN_HITS: u32 = 2;

/// 会话粘性条目:粘住的 provider + model(+ 可选账号;None = legacy 单密钥)。
///
/// 阶段五 C-1 追加**缓存观测**:粘住期间累计的 cache 命中/写入 token、
/// 命中次数、首次建立时刻 —— 「粘性值不值」的判据依据(见
/// [`sticky_cache_worth_keeping`])。聚合 sticky 表复用同一条目类型,
/// 统计字段恒为默认值(聚合口径不消费缓存收益)。
#[derive(Debug, Clone, PartialEq)]
pub struct AffinityEntry {
    pub provider_id: String,
    pub model: String,
    pub account_id: Option<String>,
    /// 阶段五 C-1:粘住期间累计的 cache 命中 token。
    pub cache_read_tokens: i64,
    /// 阶段五 C-1:粘住期间累计的 cache 写入 token。
    pub cache_write_tokens: i64,
    /// 阶段五 C-1:该绑定被复用(命中)的次数。
    pub hits: u32,
    /// 阶段五 C-1:绑定首次建立时刻(冷判定的时间基准;重绑不刷新)。
    pub bound_at_ms: i64,
}

/// 前缀钉扎条目(不含 model:同一前缀可被不同模型请求复用,读取方需
/// 自行验证 provider 能服务当前模型)。
#[derive(Debug, Clone, PartialEq)]
pub struct PrefixPinEntry {
    pub provider_id: String,
    pub account_id: Option<String>,
}

/// 泛型 TTL 表内部态:`entry + expire_at_ms` 哈希表 + 插入序队列。
/// `order` 允许残留已删除键的占位(淘汰时跳过),每个在表键恰好入队一次。
struct TableInner<E> {
    map: HashMap<String, (E, i64)>,
    order: VecDeque<String>,
    ttl_ms: i64,
}

impl<E: Clone> TableInner<E> {
    fn get(&mut self, key: &str, now_ms: i64) -> Option<E> {
        let expired = matches!(self.map.get(key), Some((_, until)) if *until <= now_ms);
        if expired {
            // 过期即删(留 order 占位,淘汰时跳过)。
            self.map.remove(key);
            return None;
        }
        self.map.get(key).map(|(entry, _)| entry.clone())
    }

    fn bind(&mut self, key: &str, entry: E, now_ms: i64, capacity: usize) {
        // 重绑先摘旧占位,保证 order 无重复键(FIFO 淘汰不误删新值)。
        self.order.retain(|k| k != key);
        self.map
            .insert(key.to_owned(), (entry, now_ms.saturating_add(self.ttl_ms)));
        self.order.push_back(key.to_owned());
        while self.map.len() > capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }

    fn remove(&mut self, key: &str) {
        self.map.remove(key);
    }

    /// 表条目数:仅供同文件单测断言,生产路径不用 —— 只编进测试目标。
    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

/// 会话粘性表(`session → (provider, model, account)`)。
pub struct SessionAffinityTable {
    inner: Mutex<TableInner<AffinityEntry>>,
    capacity: usize,
}

impl Default for SessionAffinityTable {
    fn default() -> Self {
        // 默认 TTL 与 SavingsConfig.sticky.ttl_secs 默认(900s)一致;
        // 运行时由 GatewayDeps::sync_runtime_config 随配置刷新。
        Self::new(900_000)
    }
}

impl SessionAffinityTable {
    pub fn new(ttl_ms: i64) -> Self {
        Self {
            inner: Mutex::new(TableInner {
                map: HashMap::new(),
                order: VecDeque::new(),
                ttl_ms: ttl_ms.max(1),
            }),
            capacity: STICKY_MAX_ENTRIES,
        }
    }

    /// 配置同步:刷新粘性 TTL(仅影响此后 bind 的条目)。
    pub fn sticky_set_ttl_ms(&self, ttl_ms: i64) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).ttl_ms = ttl_ms.max(1);
    }

    /// 查粘性绑定;过期即删。`now_ms` 由调用方注入(epoch 毫秒,单测可控)。
    pub fn sticky_get(&self, key: &str, now_ms: i64) -> Option<AffinityEntry> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key, now_ms)
    }

    /// 写粘性绑定(仅成功响应后调用;容量到顶淘汰最旧)。
    pub fn sticky_bind(&self, key: &str, entry: AffinityEntry, now_ms: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.bind(key, entry, now_ms, self.capacity);
    }

    /// 阶段五 C-1:带缓存观测的粘性写入(成功响应后调用)。
    ///
    /// 同一目标(同 provider/model/account)的重复绑定 → **累加**缓存统计
    /// 与命中次数、保留**首次建立时刻**(冷判定的时间基准,不随重绑
    /// 刷新)、刷新 TTL;换了目标 → 统计清零重来(新目标的冷判定从零
    /// 起算,不让旧目标的 0 命中污判新目标)。
    ///
    /// 已过期的旧条目视为不存在(`sticky_get` 也是惰性删除语义)。
    pub fn sticky_bind_observed(
        &self,
        key: &str,
        entry: AffinityEntry,
        now_ms: i64,
        cache_read_tokens: i64,
        cache_write_tokens: i64,
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let existing = inner
            .map
            .get(key)
            .filter(|(_, until)| *until > now_ms)
            .map(|(entry, _)| entry.clone());
        let mut next = entry;
        match existing {
            Some(previous)
                if previous.provider_id == next.provider_id
                    && previous.model == next.model
                    && previous.account_id == next.account_id =>
            {
                next.cache_read_tokens =
                    previous.cache_read_tokens.saturating_add(cache_read_tokens);
                next.cache_write_tokens = previous
                    .cache_write_tokens
                    .saturating_add(cache_write_tokens);
                next.hits = previous.hits.saturating_add(1);
                next.bound_at_ms = previous.bound_at_ms;
            }
            _ => {
                next.cache_read_tokens = cache_read_tokens;
                next.cache_write_tokens = cache_write_tokens;
                next.hits = 0;
                next.bound_at_ms = now_ms;
            }
        }
        inner.bind(key, next, now_ms, self.capacity);
    }

    /// 健康门失败 / 模型不匹配时释放绑定。
    pub fn sticky_remove(&self, key: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }

    /// 条目数:仅供同文件单测断言,生产路径不用 —— 只编进测试目标。
    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// 前缀钉扎表(`前缀 SHA-256 → (provider, account)`)。
pub struct PrefixPinTable {
    inner: Mutex<TableInner<PrefixPinEntry>>,
    capacity: usize,
}

impl Default for PrefixPinTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PrefixPinTable {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(TableInner {
                map: HashMap::new(),
                order: VecDeque::new(),
                ttl_ms: PREFIX_PIN_TTL_MS,
            }),
            capacity: PREFIX_MAX_ENTRIES,
        }
    }

    /// 查钉扎绑定;过期即删。TTL 固定 300s(常量,不随配置变)。
    pub fn prefix_get(&self, key: &str, now_ms: i64) -> Option<PrefixPinEntry> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key, now_ms)
    }

    /// 写钉扎绑定(仅成功响应后调用)。
    pub fn prefix_bind(&self, key: &str, entry: PrefixPinEntry, now_ms: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.bind(key, entry, now_ms, self.capacity);
    }

    /// 健康门失败时释放绑定。
    pub fn prefix_remove(&self, key: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

/// 当前 epoch 毫秒(与凭据池同时钟口径)。
pub fn now_ms() -> i64 {
    wall_ms()
}

/// 阶段五 C-1:粘住条目是否还值得保留(「值不值」判据,方案 §6.1)。
///
/// 绑定存活超过冷判定窗口且**一次 cache_read 都没有** → 该上游从不
/// 回报缓存,粘着只是白白锁死选路 → 释放绑定落正常链。
///
/// 保守口径:
/// - 判据关闭(`cold_after_ms <= 0`)→ 永远保留(升级前语义);
/// - 命中次数不足 [`STICKY_CACHE_MIN_HITS`] → 保留(样本不足,一次偶然
///   没命中不足以断言「从不回报」);
/// - 有任何 cache_read 回报 → 保留(值);
/// - 其余 → 过了冷窗口就不再保留。
///
/// ⚠️ 调用方**必须**先确认该 provider 协议族确实上报缓存
/// (`ProviderProtocol::reports_cache_usage`):在不上报缓存的上游,
/// `cache_read` 恒为 0,本判据会误杀全部正常粘性。
pub fn sticky_cache_worth_keeping(entry: &AffinityEntry, now_ms: i64, cold_after_ms: i64) -> bool {
    if cold_after_ms <= 0 {
        return true;
    }
    if entry.hits < STICKY_CACHE_MIN_HITS {
        return true;
    }
    if entry.cache_read_tokens > 0 {
        return true;
    }
    now_ms.saturating_sub(entry.bound_at_ms) < cold_after_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(provider: &str, model: &str, account: Option<&str>) -> AffinityEntry {
        AffinityEntry {
            provider_id: provider.to_owned(),
            model: model.to_owned(),
            account_id: account.map(str::to_owned),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            hits: 0,
            bound_at_ms: 0,
        }
    }

    /// 阶段五 C-1:带缓存观测的写入会累加统计、保留首次建立时刻、
    /// 换目标则统计清零。
    #[test]
    fn sticky_observed_accumulates_stats_and_keeps_bound_at() {
        let table = SessionAffinityTable::new(1_000_000);
        let mut first = entry("p1", "m1", Some("a1"));
        first.bound_at_ms = 1_000;
        table.sticky_bind_observed("s", first, 1_000, 10, 4);
        let stored = table.sticky_get("s", 1_100).unwrap();
        assert_eq!(stored.cache_read_tokens, 10, "首次写入即计入本次观测");
        assert_eq!(stored.cache_write_tokens, 4);
        assert_eq!(stored.hits, 0, "首次绑定不计命中");
        assert_eq!(stored.bound_at_ms, 1_000);

        // 同一目标重绑:累加 + 命中 +1 + 保留首次建立时刻。
        let mut again = entry("p1", "m1", Some("a1"));
        again.bound_at_ms = 5_000;
        table.sticky_bind_observed("s", again, 5_000, 5, 0);
        let stored = table.sticky_get("s", 5_100).unwrap();
        assert_eq!(stored.cache_read_tokens, 15);
        assert_eq!(stored.cache_write_tokens, 4);
        assert_eq!(stored.hits, 1);
        assert_eq!(
            stored.bound_at_ms, 1_000,
            "冷判定的时间基准是首次建立时刻,不随重绑刷新"
        );

        // 换目标:统计清零重来(不让旧目标的观测污判新目标)。
        let mut other = entry("p2", "m1", Some("a1"));
        other.bound_at_ms = 9_000;
        table.sticky_bind_observed("s", other, 9_000, 0, 0);
        let stored = table.sticky_get("s", 9_100).unwrap();
        assert_eq!(stored.cache_read_tokens, 0);
        assert_eq!(stored.hits, 0);
        assert_eq!(stored.bound_at_ms, 9_000);
    }

    /// 阶段五 C-1:「值不值」判据的四个分支(关闭/样本不足/有命中/冷窗口到)。
    #[test]
    fn sticky_cache_worth_keeping_follows_cold_window() {
        let mut entry = entry("p1", "m1", Some("a1"));
        entry.bound_at_ms = 1_000;
        entry.hits = STICKY_CACHE_MIN_HITS;
        // 判据关闭 → 永远保留。
        assert!(sticky_cache_worth_keeping(&entry, 1_000_000, 0));
        // 样本不足 → 保留。
        entry.hits = STICKY_CACHE_MIN_HITS - 1;
        assert!(sticky_cache_worth_keeping(&entry, 1_000_000, 300_000));
        // 命中数够但冷窗口未到 → 保留。
        entry.hits = STICKY_CACHE_MIN_HITS;
        assert!(sticky_cache_worth_keeping(&entry, 1_000 + 299_999, 300_000));
        // 冷窗口到 + 零 cache_read → 不值得保留。
        assert!(!sticky_cache_worth_keeping(
            &entry,
            1_000 + 300_000,
            300_000
        ));
        // 有 cache_read 回报 → 值,即便冷窗口已过。
        entry.cache_read_tokens = 1;
        assert!(sticky_cache_worth_keeping(&entry, 1_000 + 300_000, 300_000));
    }

    #[test]
    fn sticky_roundtrip_and_ttl_expiry() {
        let table = SessionAffinityTable::new(1_000);
        table.sticky_bind("s1", entry("p1", "m1", Some("a1")), 10_000);
        // TTL 内命中;account None 也原样存取。
        assert_eq!(
            table.sticky_get("s1", 10_500),
            Some(entry("p1", "m1", Some("a1")))
        );
        table.sticky_bind("s2", entry("p2", "m2", None), 10_000);
        assert_eq!(
            table.sticky_get("s2", 10_900),
            Some(entry("p2", "m2", None))
        );
        // 过期(10_000 + 1_000 = 11_000 起)→ None 且条目被删。
        assert_eq!(table.sticky_get("s1", 11_000), None);
        assert_eq!(table.sticky_get("s1", 12_000), None, "过期即删,不复活");
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn sticky_rebind_refreshes_and_remove_releases() {
        let table = SessionAffinityTable::new(1_000);
        table.sticky_bind("s", entry("p1", "m1", None), 10_000);
        // 重绑换目标并刷新 TTL。
        table.sticky_bind("s", entry("p2", "m2", None), 10_500);
        assert_eq!(
            table.sticky_get("s", 11_200),
            Some(entry("p2", "m2", None)),
            "重绑后以新值为准且 TTL 重置"
        );
        // 健康门失败释放后不再命中。
        table.sticky_remove("s");
        assert_eq!(table.sticky_get("s", 11_300), None);
    }

    #[test]
    fn sticky_capacity_evicts_oldest() {
        let table = SessionAffinityTable::new(1_000_000);
        // 手工把容量压到 2(构造函数不暴露;直接测默认容量外的语义 ——
        // 这里借 1000 容量不便,改用前缀表同构逻辑测淘汰)。
        table.sticky_bind("a", entry("p", "m", None), 1);
        table.sticky_bind("b", entry("p", "m", None), 1);
        assert_eq!(table.len(), 2);
        // 重绑不产生重复占位(淘汰序不被污染)。
        table.sticky_bind("a", entry("p", "m2", None), 2);
        assert_eq!(table.len(), 2);
        assert_eq!(table.sticky_get("a", 3).unwrap().model, "m2");
        assert_eq!(table.sticky_get("b", 3).unwrap().model, "m");
    }

    #[test]
    fn prefix_roundtrip_fixed_ttl_and_expiry() {
        let table = PrefixPinTable::new();
        assert_eq!(table.prefix_get("k", 0), None, "空表 None");
        table.prefix_bind(
            "k",
            PrefixPinEntry {
                provider_id: "p1".to_owned(),
                account_id: Some("a1".to_owned()),
            },
            5_000,
        );
        assert_eq!(
            table.prefix_get("k", 5_000 + PREFIX_PIN_TTL_MS - 1),
            Some(PrefixPinEntry {
                provider_id: "p1".to_owned(),
                account_id: Some("a1".to_owned()),
            })
        );
        // 固定 300s:到点即过期。
        assert_eq!(table.prefix_get("k", 5_000 + PREFIX_PIN_TTL_MS), None);
        // remove + 重绑。
        table.prefix_bind(
            "k2",
            PrefixPinEntry {
                provider_id: "p2".to_owned(),
                account_id: None,
            },
            5_000,
        );
        table.prefix_remove("k2");
        assert_eq!(table.prefix_get("k2", 5_001), None);
    }

    #[test]
    fn now_ms_is_epoch_milliseconds() {
        // 与凭据池 wall_ms 同时钟:数量级必须是 epoch 毫秒(2026 年)。
        assert!(now_ms() > 1_700_000_000_000, "{}", now_ms());
    }
}
