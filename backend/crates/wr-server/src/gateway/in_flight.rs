//! 在途请求注册表(调用日志「等待中/处理中」状态的内存事实源;Rust 新增,
//! 无 Python 镜像)。
//!
//! WRouter 的 `request_logs` 是「完成时一次落行」,库里没有进行中的请求。
//! 本注册表在网关入口登记每个**已认证**请求,给调用日志页补上实时的两档
//! 在途状态:
//! - 等待中(pending):已准入,尚未取得上游并发许可(限流排队/路由中);
//! - 处理中(processing):并发许可已取得,正在调用上游。
//!
//! 交接点:落库汇点 [`crate::gateway::GatewayDeps::record_log_extra`] 顺带
//! `remove` 条目 —— 完成即从「内存行」切换为「落库行」,快照与日志页的
//! 其余过滤/分页共用同一条链路(见 `RequestLogStore::query_request_logs`
//! 的 `in_flight` 参数)。思考强度也随条目暂存,在汇点并入日志 payload。
//!
//! 泄漏保护:登记/快照时清理超过 [`STALE_AFTER_SECS`] 的条目(正常请求都会
//! 经 record_log 移除;上限 1h)。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Map, Value};

/// 在途条目超过该秒数仍未落日志则视为异常,不再进入快照(1h)。
const STALE_AFTER_SECS: u64 = 3600;

/// 在途请求的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InFlightPhase {
    /// 已准入,尚未取得上游并发许可。
    Waiting,
    /// 已取得并发许可,正在调用上游。
    Processing,
}

impl InFlightPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "pending",
            Self::Processing => "processing",
        }
    }
}

/// 一次在途请求的已知事实(登记后不可变;phase 与首字节除外)。
#[derive(Debug)]
pub struct InFlightRequest {
    pub protocol: String,
    /// 客户端请求的模型名(路由解析前;聚合名/供应商前缀原样)。
    pub model: String,
    pub stream: bool,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
    /// 客户端请求体里的思考强度(归一枚举词;无则 None)。
    pub reasoning_effort: Option<String>,
    /// 登记时刻(`%Y-%m-%dT%H:%M:%SZ`,与落库行 created_at 同形)。
    pub created_at: String,
    pub started_at: Instant,
    /// 首字耗时(毫秒,登记起算):流式管线收到首个有效 chunk 时经
    /// [`InFlightRegistry::mark_first_byte`] 记入;None = 尚未出首字。
    pub first_byte_ms: Option<i64>,
    /// 20261108_01 §4.1:答案完成时刻(毫秒,登记起算;观察到语义
    /// `Finish` 时经 [`InFlightRegistry::mark_answer_end`] 记入)。
    /// 有值即冻结快照耗时 —— 拖尾排空不再让在途计时继续跳。
    pub answer_end_ms: Option<i64>,
    /// 列表行 id(负数,与 request_logs 的正 rowid 天然不冲突;登记时定死,
    /// 保证刷新间的 React key 稳定)。
    pub row_id: i64,
    pub phase: InFlightPhase,
}

#[derive(Default)]
pub struct InFlightRegistry {
    entries: Mutex<HashMap<String, InFlightRequest>>,
    seq: AtomicI64,
}

impl InFlightRegistry {
    /// 登记一个新在途请求(网关入口认证成功后调用)。
    pub fn register(&self, request_id: &str, mut request: InFlightRequest) {
        self.sweep();
        let mut entries = self.lock();
        request.row_id = self.seq.fetch_sub(1, Ordering::Relaxed) - 1;
        entries.insert(request_id.to_owned(), request);
    }

    /// 进入「处理中」(取得并发许可后调用);未登记/已移除时静默。
    /// 返回是否发生 Waiting→Processing 翻转(false = 幂等重入),供调用方
    /// 决定是否发布 `inflight-changed` 事件。
    pub fn mark_processing(&self, request_id: &str) -> bool {
        match self.lock().get_mut(request_id) {
            Some(entry) if entry.phase == InFlightPhase::Waiting => {
                entry.phase = InFlightPhase::Processing;
                true
            }
            _ => false,
        }
    }

    /// 记入首字耗时(流式管线收到首个有效 chunk 时调用;`ttft_ms` 与完成
    /// 日志同口径 —— 登记起算)。每请求只有首个调用生效;返回是否首次记入
    /// (供调用方决定是否发布 `inflight-changed` 事件)。
    pub fn mark_first_byte(&self, request_id: &str, ttft_ms: f64) -> bool {
        match self.lock().get_mut(request_id) {
            Some(entry) if entry.first_byte_ms.is_none() => {
                entry.first_byte_ms = Some(ttft_ms.max(0.0) as i64);
                true
            }
            _ => false,
        }
    }

    /// 20261108_01 §4.1:记入答案完成耗时(观察到 `Finish` 事实时调用)。
    /// 每请求只有首个调用生效;返回是否首次记入(供调用方决定是否发布
    /// `inflight-changed` 事件)。
    pub fn mark_answer_end(&self, request_id: &str, answer_end_ms: f64) -> bool {
        match self.lock().get_mut(request_id) {
            Some(entry) if entry.answer_end_ms.is_none() => {
                entry.answer_end_ms = Some(answer_end_ms.max(0.0) as i64);
                true
            }
            _ => false,
        }
    }

    /// 取走条目(落库汇点调用);返回暂存的思考强度供并入 payload。
    pub fn remove(&self, request_id: &str) -> Option<InFlightRequest> {
        self.lock().remove(request_id)
    }

    /// 当前在途快照 → 调用日志行(与 `call_log_entry` 同形;供
    /// `query_request_logs` 与正式行统一过滤/分页)。
    pub fn snapshot(&self) -> Vec<Value> {
        self.sweep();
        let entries = self.lock();
        let mut rows: Vec<(&String, &InFlightRequest)> = entries.iter().collect();
        // 新登记的在前(row_id 越小越新,负数递减;与落库行 created_at DESC
        // 视觉衔接)。
        rows.sort_by_key(|(_, entry)| entry.row_id);
        rows.into_iter()
            .map(|(request_id, entry)| snapshot_row(request_id, entry))
            .collect()
    }

    /// 清理超龄条目(泄漏保护)。
    fn sweep(&self) {
        self.lock()
            .retain(|_, entry| entry.started_at.elapsed().as_secs() < STALE_AFTER_SECS);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, InFlightRequest>> {
        self.entries.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// 一条在途快照 → 调用日志行。数值列全 0、耗时取实时 elapsed;`ok` 恒
/// false(未完成不计入成功),状态由 phase 决定。
fn snapshot_row(request_id: &str, entry: &InFlightRequest) -> Value {
    let mut row = Map::new();
    row.insert("id".to_owned(), Value::from(entry.row_id));
    row.insert(
        "requestId".to_owned(),
        Value::String(request_id.to_owned()),
    );
    row.insert("createdAt".to_owned(), Value::String(entry.created_at.clone()));
    let model = entry.model.trim();
    row.insert(
        "modelLabel".to_owned(),
        Value::String(if model.is_empty() {
            "unknown".to_owned()
        } else {
            model.to_owned()
        }),
    );
    row.insert("baseModel".to_owned(), Value::String(model.to_owned()));
    // 与 call_log_entry 同形的可选文本键:在途行一律 null(未知/未定)。
    for key in [
        "requestedModel",
        "provider",
        "proxy",
        "conversionChain",
        "apiKeyId",
        "apiKeyName",
        "endReason",
        "error",
    ] {
        row.insert(key.to_owned(), Value::Null);
    }
    if let Some(id) = &entry.api_key_id {
        row.insert("apiKeyId".to_owned(), Value::String(id.clone()));
    }
    if let Some(name) = &entry.api_key_name {
        row.insert("apiKeyName".to_owned(), Value::String(name.clone()));
    }
    row.insert("protocol".to_owned(), Value::String(entry.protocol.clone()));
    row.insert("endpointPath".to_owned(), Value::String(String::new()));
    row.insert("method".to_owned(), Value::String("POST".to_owned()));
    row.insert("path".to_owned(), Value::String(String::new()));
    row.insert("statusCode".to_owned(), Value::from(0));
    row.insert("ok".to_owned(), Value::Bool(false));
    row.insert("isStream".to_owned(), Value::Bool(entry.stream));
    for key in [
        "inputTokens",
        "outputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
        "totalTokens",
        "reasoningTokens",
    ] {
        row.insert(key.to_owned(), Value::from(0));
    }
    row.insert("costUsd".to_owned(), Value::Null);
    // 20261108_01 §4.1:答案完成即冻结耗时(后续排空不再续走);未完成时
    // 仍按实时 elapsed 供前端插值。
    row.insert(
        "durationMs".to_owned(),
        Value::from(
            entry
                .answer_end_ms
                .unwrap_or_else(|| entry.started_at.elapsed().as_millis() as i64),
        ),
    );
    row.insert(
        "answerEndMs".to_owned(),
        entry
            .answer_end_ms
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    // 20261108_01 §6.1/§13:在途行的解码/输出窗口尚未闭合 → 三个速度
    // 字段均为 null。
    for key in ["decodeTps", "outputTps", "decodeSource"] {
        row.insert(key.to_owned(), Value::Null);
    }
    // 首字耗时:流式管线出首字后经 mark_first_byte 记入(静态值);未出首字
    // 为 null(前端显示 "-",与落库的非流式行同语义)。
    row.insert(
        "firstByteMs".to_owned(),
        entry
            .first_byte_ms
            .map(|ms| Value::from(ms))
            .unwrap_or(Value::Null),
    );
    row.insert("providerAttempts".to_owned(), Value::Array(Vec::new()));
    row.insert("retryAttempts".to_owned(), Value::Array(Vec::new()));
    row.insert(
        "reasoningEffort".to_owned(),
        entry
            .reasoning_effort
            .as_ref()
            .map(|effort| Value::String(effort.clone()))
            .unwrap_or(Value::Null),
    );
    row.insert(
        "status".to_owned(),
        Value::String(entry.phase.as_str().to_owned()),
    );
    Value::Object(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(model: &str) -> InFlightRequest {
        InFlightRequest {
            protocol: "openai".to_owned(),
            model: model.to_owned(),
            stream: true,
            api_key_id: Some("k1".to_owned()),
            api_key_name: Some("golden".to_owned()),
            reasoning_effort: Some("high".to_owned()),
            created_at: "2026-09-26T00:00:00Z".to_owned(),
            started_at: Instant::now(),
            first_byte_ms: None,
            answer_end_ms: None,
            row_id: 0,
            phase: InFlightPhase::Waiting,
        }
    }

    #[test]
    fn register_snapshot_remove_lifecycle() {
        let registry = InFlightRegistry::default();
        registry.register("req-a", request("p1/m1"));
        registry.register("req-b", request("wrouter/agg"));

        let rows = registry.snapshot();
        assert_eq!(rows.len(), 2);
        // 新登记的在前,id 为负且互不相同。
        assert_eq!(rows[0]["requestId"], json!("req-b"));
        assert!(rows[0]["id"].as_i64().unwrap() < 0);
        assert_ne!(rows[0]["id"], rows[1]["id"]);
        assert_eq!(rows[0]["status"], json!("pending"));
        assert_eq!(rows[0]["isStream"], json!(true));
        assert_eq!(rows[0]["apiKeyName"], json!("golden"));
        assert_eq!(rows[0]["reasoningEffort"], json!("high"));

        // 处理中阶段翻转。
        registry.mark_processing("req-a");
        let rows = registry.snapshot();
        let a = rows.iter().find(|row| row["requestId"] == json!("req-a")).unwrap();
        assert_eq!(a["status"], json!("processing"));
        assert!(a["durationMs"].as_i64().unwrap() >= 0, "实时耗时已计入");
        // 未出首字:firstByteMs 为 null。
        assert_eq!(a["firstByteMs"], json!(null));

        // 首字节:首个调用生效并记入快照,重复调用幂等不翻转。
        assert!(registry.mark_first_byte("req-a", 842.0));
        let rows = registry.snapshot();
        let a = rows.iter().find(|row| row["requestId"] == json!("req-a")).unwrap();
        assert_eq!(a["firstByteMs"], json!(842));
        assert!(!registry.mark_first_byte("req-a", 999.0), "重复记入不翻转");
        let rows = registry.snapshot();
        let a = rows.iter().find(|row| row["requestId"] == json!("req-a")).unwrap();
        assert_eq!(a["firstByteMs"], json!(842), "首字耗时取首次值");
        // 未登记的请求静默。
        assert!(!registry.mark_first_byte("req-ghost", 10.0));

        // 移除返回条目(供汇点并入 reasoning_effort),快照随之消失。
        let removed = registry.remove("req-a").expect("remove 应取回条目");
        assert_eq!(removed.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(registry.snapshot().len(), 1);
        assert!(registry.remove("req-a").is_none(), "重复移除静默");
    }

    #[test]
    fn answer_end_freezes_snapshot_duration() {
        let registry = InFlightRegistry::default();
        registry.register("req-a", request("p1/m1"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(registry.mark_answer_end("req-a", 5.0));
        assert!(!registry.mark_answer_end("req-a", 99.0), "重复记入不翻转");
        let rows = registry.snapshot();
        assert_eq!(rows[0]["answerEndMs"], json!(5));
        assert_eq!(rows[0]["durationMs"], json!(5), "答案完成即冻结耗时");
        assert_eq!(
            rows[0]["decodeTps"],
            json!(null),
            "窗口未闭合不产出解码速度"
        );
        assert_eq!(
            rows[0]["outputTps"],
            json!(null),
            "窗口未闭合不产出输出速度"
        );
        assert_eq!(rows[0]["decodeSource"], json!(null));
        std::thread::sleep(std::time::Duration::from_millis(20));
        let rows = registry.snapshot();
        assert_eq!(rows[0]["durationMs"], json!(5), "拖尾不计入在途耗时");
        // 未登记/已移除时静默。
        assert!(!registry.mark_answer_end("req-ghost", 1.0));
    }

    #[test]
    fn stale_entries_are_swept() {
        let registry = InFlightRegistry::default();
        registry.register("req-stale", request("p1/m1"));
        // 直接把登记时刻拨回 2h 前(sweep 只看 started_at)。
        {
            let mut entries = registry.lock();
            let entry = entries.get_mut("req-stale").unwrap();
            entry.started_at = Instant::now()
                .checked_sub(std::time::Duration::from_secs(2 * 3600))
                .unwrap();
        }
        assert!(registry.snapshot().is_empty(), "超龄条目不得出现在快照");
        // sweep 已把超龄条目从表中丢弃(泄漏保护),汇点随后取走为 None。
        assert!(registry.remove("req-stale").is_none());
    }
}
