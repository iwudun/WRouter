//! Usage / Logs 组:RPC(getUsageStats/getCallLogs/getRequestLogDetail/
//! getApiKeyLastUsed)+ REST(/api/v1/usage、/api/v1/request-logs)。
//!
//! 查询语义由 R4 的 `UsageStore` / `RequestLogStore` 端口承载(R4 已逐位
//! 镜像 Python),本层做参数装配与错误形态映射:
//! - Python handler 未捕获的 ValueError → FastAPI 500 纯文本;
//! - getUsageStats 的 period 仅接受字符串(否则 "today"),filter 仅接受
//!   dict(否则 None → Value::Null)。

use std::sync::Arc;

use axum::extract::{Query, State};

use axum::Json;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::AdminState;
use super::config::plain_500;
use wr_app::ports::{RequestLogStore as _, UsageStore as _};
use wr_infra::sqlite::SqliteStore;

/// RPC `getUsageStats([period?, filter?])` / REST GET /api/v1/usage(period="today")。
pub async fn svc_usage_stats(store: &Arc<SqliteStore>, args: &[Value]) -> Result<Value, String> {
    // Python: str(args[0]) if isinstance(args[0], str) else "today"。
    let period = args
        .first()
        .and_then(Value::as_str)
        .unwrap_or("today")
        .to_owned();
    // Python: args[1] if len(args) > 1 and isinstance(args[1], dict) else None。
    let filter = match args.get(1) {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        _ => Value::Null,
    };
    store
        .usage_summary(Some(&period), &filter)
        .await
        .map_err(|e| e.to_string())
}

/// RPC `getCallLogs([filter?])`:filter 非 dict → None → Null。
///
/// 在途请求(等待中/处理中)来自网关注册表(`deps.in_flight`),以投影
/// 后的行并入查询最前,与落库行共用过滤/计数/分页。
pub async fn svc_call_logs(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    let filter = match args.first() {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        _ => Value::Null,
    };
    let in_flight = state.deps.in_flight_rows();
    state
        .store
        .query_request_logs(&filter, &in_flight)
        .await
        .map_err(|e| e.to_string())
}

/// RPC `getRequestLogDetail([{request_id?|id?}] 或 str)`:
/// request_id 优先,id 为正数时 rowid 兜底;缺参/未找到 → Err(500 纯文本)。
pub async fn svc_request_log_detail(
    store: &Arc<SqliteStore>,
    args: &[Value],
) -> Result<Value, String> {
    let payload = args.first().cloned().unwrap_or(Value::Null);
    let mut request_id = String::new();
    let mut numeric_id: Option<i64> = None;
    match &payload {
        Value::String(s) => request_id = s.clone(),
        Value::Object(map) => {
            request_id = map
                .get("request_id")
                .or_else(|| map.get("id"))
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            if let Some(raw) = map.get("id") {
                // Python: isinstance(raw, (int, float)) and raw > 0。
                if let Some(n) = raw.as_i64().or_else(|| raw.as_f64().map(|f| f as i64))
                    && n > 0
                {
                    numeric_id = Some(n);
                }
            }
        }
        _ => {}
    }
    if request_id.is_empty() && numeric_id.is_none() {
        return Err("getRequestLogDetail requires request_id".to_owned());
    }
    let mut row = if request_id.is_empty() {
        None
    } else {
        store.get_request_log(&request_id).await.unwrap_or_default()
    };
    if row.is_none()
        && let Some(nid) = numeric_id
    {
        row = store
            .get_request_log_by_rowid(nid)
            .await
            .unwrap_or_default();
    }
    row.ok_or_else(|| format!("request log '{}' not found", request_id))
}

/// RPC `getRequestLogTrace({request_id, offset?, limit?, kind?})`。
///
/// `kind` 过滤(D4):`attempt` 拿执行尝试摘要,`sse_event` 拿原始流事件,
/// 不传则按 ordinal 混排。两类数据可各自独立翻页。
pub async fn svc_request_log_trace(store: &Arc<SqliteStore>, args: &[Value]) -> Result<Value, String> {
    let Some(Value::Object(map)) = args.first() else { return Err("getRequestLogTrace requires request_id".to_owned()); };
    let request_id = map.get("request_id").and_then(Value::as_str).unwrap_or("");
    if request_id.is_empty() { return Err("getRequestLogTrace requires request_id".to_owned()); }
    let offset = map.get("offset").and_then(Value::as_i64).unwrap_or(0).max(0);
    let limit = map.get("limit").and_then(Value::as_i64).unwrap_or(100).clamp(1, 200);
    let kind = map.get("kind").and_then(Value::as_str);
    let page = store.get_request_log_trace(request_id, offset, limit, kind).await.map_err(|error| error.to_string())?;
    Ok(serde_json::json!({"items": page.items, "nextOffset": page.next_offset, "total": page.total}))
}

/// RPC `getApiKeyLastUsed()`。
pub async fn svc_last_used(store: &Arc<SqliteStore>) -> Result<Value, String> {
    store
        .last_used_by_api_key()
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// REST 门面
// ---------------------------------------------------------------------------

pub async fn rest_usage(State(state): State<AdminState>) -> Response {
    match svc_usage_stats(&state.store, &[]).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => plain_500(),
    }
}

pub async fn rest_request_logs(
    State(state): State<AdminState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let limit: i64 = query
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    match state.store.list_request_logs(limit).await {
        Ok(rows) => Json(Value::Array(rows)).into_response(),
        Err(_) => plain_500(),
    }
}
