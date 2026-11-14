//! 凭据池状态 RPC(方案 20260905_02 K5;Rust 新增能力,无 Python 镜像)。
//!
//! - `getProviderCredentialStats`:每供应商各账号的健康快照
//!   `{accountId, state, cooldownReason?, cooldownRemainingMs,
//!   lastStatusCode?}`。state 数据源:池内存态(只读)+ 配置快照的
//!   `provider.accounts`(disabled 判定与账号清单);
//! - `clearCredentialCooldown`:立即解除指定账号的冷却(管理面
//!   "立即解冷却"操作的后端)。
//!
//! 冷却态是进程内存(重启清零,方案 §5.3 已登记),stats 只反映当前
//! 进程的运行态。

use serde_json::{Map, Value, json};

use super::AdminState;

/// `getProviderCredentialStats`:args[0] 可选 `{ providerId }` 过滤
/// (缺省返回全部配置了凭据池的供应商)。
pub async fn svc_credential_stats(state: &AdminState, args: &[Value]) -> Result<Value, String> {
    let provider_filter = args
        .first()
        .and_then(Value::as_object)
        .and_then(|payload| payload.get("providerId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let snapshot = state
        .deps
        .snapshot()
        .await
        .map_err(|e| format!("getProviderCredentialStats snapshot failed: {e}"))?;
    let pool = &state.deps.credentials;
    let mut providers = Vec::new();
    for provider in &snapshot.providers {
        if let Some(filter) = &provider_filter
            && provider.id != *filter
        {
            continue;
        }
        // legacy 单 key 形态无凭据池,不出现在 stats 里。
        if provider.accounts.is_empty() {
            continue;
        }
        let accounts: Vec<Value> = provider
            .accounts
            .iter()
            .map(|account| {
                // 阶段五 C-2:模型级冷却(限速/上游故障)也要在管理面可见 ——
                // 只看账号级会让用户以为「明明在冷却却显示可用」。
                let cooling_ms = pool.account_cooling_remaining_ms(&provider.id, &account.id);
                let cooling = cooling_ms > 0;
                let state_str = if !account.enabled {
                    "disabled"
                } else if cooling {
                    "cooling"
                } else {
                    "ok"
                };
                let mut item = Map::new();
                item.insert("accountId".to_owned(), Value::String(account.id.clone()));
                item.insert("state".to_owned(), Value::String(state_str.to_owned()));
                item.insert("cooldownRemainingMs".to_owned(), Value::from(cooling_ms));
                if cooling {
                    if let Some(reason) = pool.cooldown_reason(&provider.id, &account.id) {
                        item.insert("cooldownReason".to_owned(), Value::String(reason));
                    }
                }
                if let Some(status) = pool.last_status_code(&provider.id, &account.id) {
                    item.insert("lastStatusCode".to_owned(), Value::from(status));
                }
                Value::Object(item)
            })
            .collect();
        providers.push(json!({
            "providerId": provider.id,
            "accounts": accounts,
        }));
    }
    Ok(json!({ "providers": providers }))
}

/// `clearCredentialCooldown`:args[0] 必填 `{ providerId, accountId }`。
/// 幂等:对未在冷却中的账号调用同样是成功(池侧 remove 无键即 no-op)。
pub async fn svc_clear_credential_cooldown(
    state: &AdminState,
    args: &[Value],
) -> Result<Value, String> {
    let payload = args
        .first()
        .and_then(Value::as_object)
        .ok_or("clearCredentialCooldown expects { providerId, accountId }")?;
    let provider_id = payload
        .get("providerId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let account_id = payload
        .get("accountId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if provider_id.is_empty() || account_id.is_empty() {
        return Err(
            "clearCredentialCooldown requires non-empty providerId and accountId".to_owned(),
        );
    }
    state
        .deps
        .credentials
        .clear_cooldown(provider_id, account_id);
    Ok(json!({
        "providerId": provider_id,
        "accountId": account_id,
        "cleared": true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// args[0] 形态:对象包一层数组。
    fn args(payload: Value) -> Vec<Value> {
        vec![payload]
    }

    #[test]
    fn provider_filter_shape() {
        // 过滤参数提取逻辑的纯形态检查(真正快照路径走 RPC 集成)。
        let payload = json!({"providerId": "  openai-main "});
        let filter = args(payload)
            .first()
            .and_then(Value::as_object)
            .and_then(|m| m.get("providerId"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        assert_eq!(filter.as_deref(), Some("openai-main"));
        // 缺省/空串 → None(不过滤)。
        for junk in [
            json!({}),
            json!({"providerId": ""}),
            json!({"providerId": 3}),
        ] {
            let filter = args(junk)
                .first()
                .and_then(Value::as_object)
                .and_then(|m| m.get("providerId"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            assert_eq!(filter, None);
        }
    }

    #[test]
    fn clear_requires_both_ids() {
        // id 提取与空判定的形态检查。
        let payload = json!({"providerId": "p", "accountId": " "});
        let account_id = payload
            .get("accountId")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        assert!(account_id.is_empty(), "空白 accountId 必须被拒");
    }
}
