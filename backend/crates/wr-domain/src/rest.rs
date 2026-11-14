//! 阶段五 C-2:上游失败的**语义类别**与 rest(冷却)时长判定。
//!
//! rest 不是
//! 「一律 60s」,而是**按失败类别定时长** ——
//! - 配额耗尽(quota):用厂商给的 reset 时间,上限 8 天;
//! - 余额耗尽(credit):30 分钟;
//! - 账号受限(restricted):2 小时;
//! - 需要验证(verify):30 分钟;
//! - 鉴权失败(auth):凭据级失效(300s,与既有分档一致);
//! - 瞬时限速(rate limit)/上游故障:短暂(60s,可被 Retry-After 覆盖);
//! - 未知错误:指数梯子 1m → 10m。
//!
//! 读侧判定只做**分类**(纯函数 + 方言正则表,照抄
//! `gateway::is_context_overflow_body` 的写法),冷却表的写入与时长
//! 计算在 `wr-server::gateway::credential_pool`。
//!
//! 分类是**保守**的:正文没给出足够线索时归到该状态码的默认类别
//! (429 → RateLimit),不做过度推断 —— 误分类会让一个还活着的账号
//! 被锁几小时,代价远大于「少判一类」。

use std::sync::OnceLock;

/// 失败类别(语义层,不是 HTTP 状态码的同义改写)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestClass {
    /// 配额耗尽(quota exceeded):恢复时间由厂商给出,通常以小时/天计。
    Quota,
    /// 余额/额度耗尽(credit exhausted / payment required)。
    Credit,
    /// 账号被限制(suspended / restricted)。
    Restricted,
    /// 需要人工验证(verify / captcha)。
    Verify,
    /// 鉴权失败(key 失效 / 无权限):凭据级失效。
    Auth,
    /// 瞬时限速(rate limit):短暂恢复。
    RateLimit,
    /// 上游服务错误(5xx)。
    ServerError,
    /// 未知错误:走指数梯子。
    Unknown,
}

impl RestClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::Credit => "credit",
            Self::Restricted => "restricted",
            Self::Verify => "verify",
            Self::Auth => "auth",
            Self::RateLimit => "rate_limit",
            Self::ServerError => "server_error",
            Self::Unknown => "unknown",
        }
    }

    /// 该类别是否**账号级**(整把 key 停用),还是**模型级**(只影响
    /// 该账号下的这一个模型)。
    ///
    /// 账号级:key 失效、账号配额/余额/受限/待验证 —— 这些与具体模型
    /// 无关,换模型也用不了。
    /// 模型级:限速、上游故障、未知错误 —— 只说明「这个模型这条路」
    /// 暂时不通,同一账号的其它模型仍可用。
    pub fn is_account_scoped(self) -> bool {
        matches!(
            self,
            Self::Quota | Self::Credit | Self::Restricted | Self::Verify | Self::Auth
        )
    }
}

/// 语义分类用的方言正则表(懒编译;照抄 `is_context_overflow_body` 的
/// 模式:命中即真,不做上下文消歧)。
fn patterns() -> &'static [(RestClass, regex::Regex)] {
    static PATTERNS: OnceLock<Vec<(RestClass, regex::Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (RestClass::Quota, r"quota"),
            (RestClass::Quota, r"配额"),
            (
                RestClass::Credit,
                r"insufficient[_ ]?(credit|funds|balance)",
            ),
            (
                RestClass::Credit,
                r"(credit|balance|billing) (is )?(exhausted|insufficient|depleted)",
            ),
            (RestClass::Credit, r"billing"),
            (RestClass::Credit, r"余额(不足|耗尽)"),
            (RestClass::Credit, r"欠费"),
            (
                RestClass::Restricted,
                r"(has been |is |was )?suspend(ed|ion)",
            ),
            (RestClass::Restricted, r"suspend(ed)? account"),
            (RestClass::Restricted, r"account (is )?restricted"),
            (
                RestClass::Restricted,
                r"(access|account) (denied|disabled|deactivated)",
            ),
            (RestClass::Restricted, r"账号(被)?(限制|封禁|禁用)"),
            (RestClass::Verify, r"captcha"),
            (
                RestClass::Verify,
                r"(requires? |needs? )?(verification|verify)",
            ),
            (RestClass::Verify, r"需要(人工)?验证"),
        ]
        .into_iter()
        .filter_map(|(class, pattern)| {
            regex::Regex::new(&format!("(?i){pattern}"))
                .ok()
                .map(|compiled| (class, compiled))
        })
        .collect()
    })
}

fn body_matches(class: RestClass, body: &str) -> bool {
    if body.is_empty() {
        return false;
    }
    patterns()
        .iter()
        .filter(|(candidate, _)| *candidate == class)
        .any(|(_, compiled)| compiled.is_match(body))
}

/// 判定一次上游失败的语义类别。
///
/// - `status`:上游 HTTP 状态码;
/// - `body`:上游错误正文(原文;厂商把配额/余额说明写在正文里);
/// - `error_type`:归一化错误的 `type`(如 `rate_limit_error`);
/// - `code`:归一化错误的 `code`(如 `insufficient_quota`)。
///
/// 保守口径:正文/码没给出线索时按状态码归默认类别。
pub fn classify_rest_class(
    status: u16,
    body: &str,
    error_type: &str,
    code: Option<&str>,
) -> RestClass {
    let haystack = format!("{body}\n{error_type}\n{}", code.unwrap_or_default());
    let has = |class: RestClass| body_matches(class, &haystack);

    // 402:付款/额度问题,正文往往不给线索。
    if status == 402 {
        return RestClass::Credit;
    }
    // 401/403:凭据级失效,但正文可能把它细化成受限/待验证。
    if status == 401 || status == 403 {
        if has(RestClass::Verify) {
            return RestClass::Verify;
        }
        if has(RestClass::Restricted) {
            return RestClass::Restricted;
        }
        if has(RestClass::Credit) {
            return RestClass::Credit;
        }
        return RestClass::Auth;
    }
    if status == 429 {
        // 429 语义最杂:配额耗尽与瞬时限速必须分开 —— 前者要等几小时,
        // 后者等 60s 就够。
        if has(RestClass::Quota) {
            return RestClass::Quota;
        }
        if has(RestClass::Credit) {
            return RestClass::Credit;
        }
        if has(RestClass::Restricted) {
            return RestClass::Restricted;
        }
        if has(RestClass::Verify) {
            return RestClass::Verify;
        }
        return RestClass::RateLimit;
    }
    if status >= 500 {
        return RestClass::ServerError;
    }
    if status == 400 {
        // 400 多为请求形状问题,不是账号问题 → 归未知(短梯子)。
        return RestClass::Unknown;
    }
    RestClass::Unknown
}

/// 未知错误的指数梯子:第 1 次 1 分钟,之后每次翻倍,上限 10 分钟。
///
/// 口径(rest 未知错误指数梯子 1m→10m);`failures` 是该账号
/// 的**连续**失败次数(成功即清零)。
pub fn rest_ladder_ms(failures: u32) -> i64 {
    const BASE_MS: i64 = 60_000;
    const MAX_MS: i64 = 600_000;
    // 第 1 次失败 = 1 分钟,之后每次翻倍。
    let step = failures.saturating_sub(1).min(16);
    BASE_MS.saturating_mul(1i64 << step).min(MAX_MS)
}

/// 冷却时长的语义类别基准值(毫秒)。
///
/// `Quota` 用厂商 reset 时间(见 [`rest_duration_ms`]),这里给的是
/// 「拿不到 reset 时间」时的兜底。
pub fn rest_base_ms(class: RestClass) -> i64 {
    match class {
        RestClass::Quota => 60_000,
        RestClass::Credit => 30 * 60_000,
        RestClass::Restricted => 2 * 3_600_000,
        RestClass::Verify => 30 * 60_000,
        RestClass::Auth => 300_000,
        RestClass::RateLimit => 60_000,
        RestClass::ServerError => 60_000,
        RestClass::Unknown => 60_000,
    }
}

/// 配额类失败的重置时长上限(毫秒,8 天)。
pub const QUOTA_RESET_MAX_MS: i64 = 8 * 86_400_000;

/// 一次失败的完整 rest 信号(分类 + 厂商给出的重置时长)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureSignal {
    /// 语义类别。
    pub class: RestClass,
    /// 厂商给出的重置/重试时长(毫秒):`Retry-After` 头或正文解析。
    pub reset_ms: Option<i64>,
}

impl FailureSignal {
    pub fn new(class: RestClass, reset_ms: Option<i64>) -> Self {
        Self { class, reset_ms }
    }
}

impl Default for FailureSignal {
    fn default() -> Self {
        Self {
            class: RestClass::Unknown,
            reset_ms: None,
        }
    }
}

/// 按类别计算冷却时长(毫秒)。
///
/// - `Quota`:优先用厂商 reset 时间(上限 8 天);没有则走梯子;
/// - `RateLimit`:用厂商 reset 时间(上限 24h,与既有 Retry-After 行为
///   一致);没有则 60s;
/// - 其它类别:取 [`rest_base_ms`] 的语义基准;
/// - `Unknown`:走指数梯子。
///
/// `failures` 是该账号的连续失败次数(1 起)。
pub fn rest_duration_ms(class: RestClass, reset_ms: Option<i64>, failures: u32) -> i64 {
    match class {
        RestClass::Quota => reset_ms
            .filter(|ms| *ms > 0)
            .map(|ms| ms.min(QUOTA_RESET_MAX_MS))
            .unwrap_or_else(|| rest_ladder_ms(failures)),
        RestClass::RateLimit => reset_ms
            .filter(|ms| *ms > 0)
            .map(|ms| ms.min(86_400_000))
            .unwrap_or_else(|| rest_base_ms(class)),
        RestClass::Unknown => rest_ladder_ms(failures),
        _ => rest_base_ms(class),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_and_rate_limit_are_distinguished() {
        // 配额耗尽:正文明说 quota → Quota(等小时级,不是 60s)。
        assert_eq!(
            classify_rest_class(429, "You exceeded your current quota", "", None),
            RestClass::Quota
        );
        // 瞬时限速:无配额线索 → RateLimit。
        assert_eq!(
            classify_rest_class(429, "Rate limit exceeded for requests", "", None),
            RestClass::RateLimit
        );
        // 厂商码也算线索。
        assert_eq!(
            classify_rest_class(429, "", "", Some("insufficient_quota")),
            RestClass::Quota
        );
    }

    #[test]
    fn auth_failures_are_refined_by_body() {
        assert_eq!(
            classify_rest_class(401, "invalid api key", "", None),
            RestClass::Auth
        );
        assert_eq!(
            classify_rest_class(403, "This account has been suspended", "", None),
            RestClass::Restricted
        );
        assert_eq!(
            classify_rest_class(403, "Please complete verification", "", None),
            RestClass::Verify
        );
        assert_eq!(classify_rest_class(402, "", "", None), RestClass::Credit);
    }

    #[test]
    fn account_scope_matches_class() {
        assert!(RestClass::Quota.is_account_scoped());
        assert!(RestClass::Credit.is_account_scoped());
        assert!(RestClass::Auth.is_account_scoped());
        assert!(!RestClass::RateLimit.is_account_scoped());
        assert!(!RestClass::ServerError.is_account_scoped());
        assert!(!RestClass::Unknown.is_account_scoped());
    }

    #[test]
    fn duration_follows_class_and_reset_hint() {
        // 配额:用厂商 reset(上限 8 天)。
        assert_eq!(
            rest_duration_ms(RestClass::Quota, Some(3_600_000), 1),
            3_600_000
        );
        assert_eq!(
            rest_duration_ms(RestClass::Quota, Some(30 * 86_400_000), 1),
            QUOTA_RESET_MAX_MS
        );
        // 配额但没有 reset → 梯子。
        assert_eq!(rest_duration_ms(RestClass::Quota, None, 1), 60_000);
        // 限速:60s 基准,Retry-After 覆盖(上限 24h)。
        assert_eq!(rest_duration_ms(RestClass::RateLimit, None, 1), 60_000);
        assert_eq!(
            rest_duration_ms(RestClass::RateLimit, Some(120_000), 1),
            120_000
        );
        // 语义基准。
        assert_eq!(rest_duration_ms(RestClass::Credit, None, 1), 1_800_000);
        assert_eq!(rest_duration_ms(RestClass::Restricted, None, 1), 7_200_000);
        assert_eq!(rest_duration_ms(RestClass::Verify, None, 1), 1_800_000);
        assert_eq!(rest_duration_ms(RestClass::Auth, None, 1), 300_000);
        assert_eq!(rest_duration_ms(RestClass::ServerError, None, 1), 60_000);
    }

    #[test]
    fn unknown_failures_follow_the_ladder() {
        assert_eq!(rest_ladder_ms(1), 60_000);
        assert_eq!(rest_ladder_ms(2), 120_000);
        assert_eq!(rest_ladder_ms(3), 240_000);
        assert_eq!(rest_ladder_ms(4), 480_000);
        assert_eq!(rest_ladder_ms(5), 600_000, "上限 10 分钟");
        assert_eq!(rest_ladder_ms(50), 600_000, "不因次数过大而溢出");
    }
}
