//! 调用方身份归一(方案 20261104_01 阶段三 · B-1b):把入站 User-Agent
//! 收敛成稳定的客户端 id。
//!
//! 背景:用量统计的 `client` 维度原本直接取**原始 UA 串**
//! (`wr-server` 的 `gateway::forward::sse::client_label`),客户端升级后
//! UA 里的版本号变了 → 同一客户端在统计里裂成多条
//! (`ZCode/3.14.5 ai-sdk/…` 与 `ZCode/3.15.3 ai-sdk/…` 是两个维度行,
//! 排行榜与用量卡各显示一条)。
//!
//! 口径(**全仓单一口径,只此一处实现**):
//!
//! 1. 取 UA 的**首个空白分隔 token**,再按**首个 `/`** 切 → **产品名**
//!    (`ZCode/3.15.3 ai-sdk/provider-utils/4.0.39 runtime/node.js/24` →
//!    `ZCode`;`claude-cli/2.1.123 (external, cli)` → `claude-cli`;
//!    `undici` → `undici`);
//! 2. 产品名(小写)命中**已知客户端别名表** → 换成规范展示名;
//! 3. 未命中 → 用产品名原样(保留原始大小写);
//! 4. 取不到产品名 → [`UNKNOWN_CLIENT`]。
//!
//! **别名表从客户端指纹预设派生**,不另维护手写表:预设里的
//! `user-agent` 就是这些客户端的真实 UA 形态、预设的 `label` 就是规范
//! 展示名 → 预设升级,别名表自动跟随(见 [`alias_table`] 的注释)。
//!
//! **幂等**:归一后的 id 再归一仍是自身 → 写侧存归一 id、历史行存原始 UA
//! 两种形态能在**读侧**用同一个函数收敛到同一维度,既不用给
//! `usage_rollup` 加列,也不用做数据迁移。
//!
//! 与出站指纹层的分工:[`super::client_fingerprint`]管的是「我们以什么
//! 身份出站」,本模块管的是「谁在打我们的网关」—— 两者都读 UA,但方向
//! 相反、互不覆盖。

use std::collections::HashMap;
use std::sync::OnceLock;

use super::client_fingerprint::builtin_presets;

/// 归一不出产品名时的占位 client id(与写入侧「没有 UA 头 → unknown」的
/// 口径一致)。
pub const UNKNOWN_CLIENT: &str = "unknown";

/// 产品名长度上限(字节)。UA 是客户端自报的任意字符串,产品名理论上可以
/// 很长 —— 但真实客户端的产品名都很短,截断只用于防单个畸形 UA 把
/// 维度基数撑爆。
const MAX_PRODUCT_BYTES: usize = 64;

/// 已知客户端别名表:小写键 → 规范展示名。
///
/// 键有两类:
/// - **产品名键**:预设 UA 的产品名(`zcode` / `claude-cli` /
///   `codex_cli_rs` / `opencode`)→ 命中即换成预设的 `label`;
/// - **规范名键**:预设 `label` 的小写形式(`claude code` / `codex cli`)→
///   指向自己。**必须有这一类**:规范名里有空格,而第 1 步按空白切 token
///   会把 `Claude Code` 截成 `Claude` → 没有规范名键,归一就不幂等,
///   「按客户端筛选」会拿归一 id 再归一一次、两侧对不上(见
///   [`normalize_client_identity`] 的幂等说明)。
fn alias_table() -> &'static HashMap<String, &'static str> {
    static TABLE: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table: HashMap<String, &'static str> = HashMap::new();
        for preset in builtin_presets() {
            let label = preset.label.as_str();
            if label.is_empty() {
                continue;
            }
            if let Some(user_agent) = preset
                .headers
                .get("user-agent")
                .and_then(|value| value.as_str())
            {
                let product = product_name(user_agent);
                if !product.is_empty() {
                    table.insert(product.to_ascii_lowercase(), label);
                }
            }
            table.insert(label.to_ascii_lowercase(), label);
        }
        table
    })
}

/// 查别名表(小写匹配)。
fn alias_lookup(text: &str) -> Option<&'static str> {
    alias_table().get(&text.to_ascii_lowercase()).copied()
}

/// 取 UA 的产品名:首个空白分隔 token → 切首个 `/`。
fn product_name(raw: &str) -> String {
    let token = raw.split_whitespace().next().unwrap_or_default();
    let product = token.split('/').next().unwrap_or_default();
    truncate_at_char_boundary(product, MAX_PRODUCT_BYTES)
}

/// 按字节上限截断(不切碎 UTF-8 多字节字符)。
fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// 把原始 User-Agent 归一成稳定的客户端 id。
///
/// **幂等**:`normalize(normalize(ua)) == normalize(ua)`。读侧与过滤侧都
/// 走这个函数 → 写侧存归一 id、历史行存原始 UA 的两种形态在比较时
/// 仍然相等(过滤条件也要归一,不能拿原始 UA 直接比)。
pub fn normalize_client_identity(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return UNKNOWN_CLIENT.to_owned();
    }
    // 整串先查一次:已经归一过的 id(含空格,如 `Claude Code`)在这里命中,
    // 不会被下面的「按空白切 token」截断 → 幂等的关键。
    if let Some(label) = alias_lookup(trimmed) {
        return label.to_owned();
    }
    let product = product_name(trimmed);
    if product.is_empty() {
        return UNKNOWN_CLIENT.to_owned();
    }
    match alias_lookup(&product) {
        Some(label) => label.to_owned(),
        None => product,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 核心诉求:同一客户端换版本号 → 同一个 id --------------------------

    #[test]
    fn same_client_with_different_versions_collapses_to_one_id() {
        let user_agents = [
            "ZCode/3.14.5 ai-sdk/provider-utils/4.0.39 runtime/node.js/24",
            "ZCode/3.15.3 ai-sdk/provider-utils/4.0.39 runtime/node.js/24",
            "ZCode/3.16.3 ai-sdk/provider-utils/4.0.39 runtime/node.js/24",
            "ZCode/3.15.1 ai/6.0.193 ai-sdk/provider-utils/4.0.27 runtime/node.js/24",
        ];
        let ids: Vec<String> = user_agents
            .iter()
            .map(|ua| normalize_client_identity(ua))
            .collect();
        assert_eq!(ids[0], "ZCode");
        assert!(
            ids.iter().all(|id| id == &ids[0]),
            "同客户端多版本必须归一成同一个 id: {ids:?}"
        );
    }

    #[test]
    fn different_clients_stay_separate() {
        let zcode = normalize_client_identity("ZCode/3.15.3 ai-sdk/provider-utils/4.0.39");
        let undici = normalize_client_identity("undici");
        let curl = normalize_client_identity("curl/8.5.0");
        assert_ne!(zcode, undici);
        assert_ne!(zcode, curl);
        assert_ne!(undici, curl);
    }

    // ---- 已知客户端的规范名 -----------------------------------------------

    #[test]
    fn known_clients_use_canonical_labels() {
        assert_eq!(
            normalize_client_identity("claude-cli/2.1.123 (external, cli)"),
            "Claude Code"
        );
        assert_eq!(
            normalize_client_identity("codex_cli_rs/0.50.0 (Windows 10.0.28000; x86_64) unknown"),
            "Codex CLI"
        );
        assert_eq!(normalize_client_identity("opencode/1.18.3"), "OpenCode");
        assert_eq!(
            normalize_client_identity("ZCode/3.15.3 ai-sdk/provider-utils/4.0.39"),
            "ZCode"
        );
    }

    #[test]
    fn unknown_clients_keep_product_name_without_version() {
        assert_eq!(normalize_client_identity("undici"), "undici");
        assert_eq!(normalize_client_identity("curl/8.5.0"), "curl");
        assert_eq!(
            normalize_client_identity("python-requests/2.31.0"),
            "python-requests"
        );
        assert_eq!(
            normalize_client_identity("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"),
            "Mozilla"
        );
    }

    // ---- 边界 -------------------------------------------------------------

    #[test]
    fn empty_or_broken_user_agent_falls_back_to_unknown() {
        assert_eq!(normalize_client_identity(""), "unknown");
        assert_eq!(normalize_client_identity("   "), "unknown");
        assert_eq!(normalize_client_identity("/"), "unknown");
        assert_eq!(normalize_client_identity("unknown"), "unknown");
    }

    #[test]
    fn normalization_is_idempotent() {
        // 幂等是「写侧存归一 id、历史行存原始 UA」能在读侧收敛的前提,
        // 也是「按客户端筛选」两侧能对上的前提。
        for user_agent in [
            "ZCode/3.15.3 ai-sdk/provider-utils/4.0.39 runtime/node.js/24",
            "claude-cli/2.1.123 (external, cli)",
            "codex_cli_rs/0.50.0 (Windows 10.0.28000; x86_64) unknown",
            "opencode/1.18.3",
            "undici",
            "curl/8.5.0",
            "",
        ] {
            let once = normalize_client_identity(user_agent);
            assert_eq!(
                normalize_client_identity(&once),
                once,
                "归一必须幂等: {user_agent:?} → {once:?}"
            );
        }
    }

    #[test]
    fn oversized_product_name_is_truncated() {
        let user_agent = format!("{}/1.0", "x".repeat(200));
        let id = normalize_client_identity(&user_agent);
        assert!(id.len() <= MAX_PRODUCT_BYTES, "{id}");
    }

    #[test]
    fn leading_whitespace_does_not_change_the_id() {
        assert_eq!(
            normalize_client_identity("  ZCode/3.15.3 ai-sdk/provider-utils/4.0.39"),
            "ZCode"
        );
    }
}
