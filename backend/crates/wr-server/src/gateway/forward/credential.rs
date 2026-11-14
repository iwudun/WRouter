// 账号凭据注入(镜像 ProviderSelector._pick_account)
// 20261013_01 C1 自 forward.rs 纯搬移;经 `use super::*` 复用共享类型。

use super::*;

/// 账号凭据注入(`ProviderSelector.select_provider` 的 `_pick_account` 步):
/// provider 配置了凭据池时按策略(`credential_strategy`)选号,把 secrets
/// 里的密钥替换进 provider;池空/无可用账号/存储错误 → 原样返回。
///
/// 返回 (注入后的 provider, 命中的账号 id)。账号 id 仅在**密钥真的注入**
/// 时为 `Some`(空密钥不覆盖 legacy 凭据,与 `with_account_credential` 的
/// `if stored:` 一致)——x-wr-account 头与凭据结果记录都以它为准。
/// `session_key` 是 K4 的会话键(sticky 策略用),见
/// `wr_domain::gateway::extract_session_key`。
/// `model` 是本次尝试的模型名(阶段五 C-2:模型级冷却只剔除该模型的
/// 候选,同一账号的其它模型仍可用)。
pub(super) async fn inject_account_credential(
    deps: &GatewayDeps,
    provider: Provider,
    session_key: Option<&str>,
    model: &str,
) -> (Provider, Option<String>) {
    if provider.accounts.is_empty() {
        return (provider, None);
    }
    let mut rng = crate::gateway::credential_pool::WeightedRng::from_entropy();
    let Some(index) = crate::gateway::credential_pool::pick_account_with_model(
        &mut || rng.next_f64(),
        &deps.credentials,
        &provider,
        session_key,
        model,
    ) else {
        return (provider, None);
    };
    let Some(account) = provider.accounts.get(index).cloned() else {
        return (provider, None);
    };
    inject_account_credential_for_account(deps, provider, Some(&account.id)).await
}

/// 指定账号注入(20260905_03 S2 粘性/钉扎路径用;绕过策略选号):
/// 只注入该账号的密钥;账号不存在/disabled/存储错误/密钥为空 → 原样
/// 返回 legacy 凭据,账号 id 记 `None`(与 `with_account_credential`
/// 的 `if stored:` 一致,空密钥不覆盖 legacy 单密钥)。
pub(super) async fn inject_account_credential_for_account(
    deps: &GatewayDeps,
    provider: Provider,
    account_id: Option<&str>,
) -> (Provider, Option<String>) {
    let Some(account_id) = account_id else {
        return (provider, None);
    };
    let Some(account) = provider
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .cloned()
    else {
        return (provider, None);
    };
    if !account.enabled {
        return (provider, None);
    }
    let Ok(secret) = deps.store.account_secret(&provider.id, &account.id).await else {
        return (provider, None);
    };
    if secret.is_empty() {
        return (provider, None);
    }
    (
        crate::gateway::credential_pool::with_account_credential(provider, Some(&account), &secret),
        Some(account.id),
    )
}
