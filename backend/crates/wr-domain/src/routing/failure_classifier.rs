//! `classifyRouteFailure` + `shouldFallback`(mode-aware)
//! (镜像 Python `domain/routing/failure_classifier.py`,R3.4)。
//!
//! 分类函数与 mode 判定与 `domain/gateway/policies.py` 的 Python 实现
//! 同源,R2 已落在 [`crate::gateway`];本模块是计划文件名的镜像落点,
//! re-export 同一实现(单一事实源,防止两套分类漂移),并承载 C4 的
//! 命名断言(熔断判定不含 501 / 计划层回退语义)。

pub use crate::gateway::{classify_route_failure, should_fallback_mode};

use crate::routing::contracts::RouteFailureClass;

/// 与 [`classify_route_failure`] 同语义的类型化变体(供匹配式代码使用)。
pub fn classify(status: u16) -> RouteFailureClass {
    match classify_route_failure(status) {
        "client" => RouteFailureClass::Client,
        "rate-limit" => RouteFailureClass::RateLimit,
        "retryable" => RouteFailureClass::Retryable,
        _ => RouteFailureClass::Server,
    }
}

/// 20260905_03 S7:带正文的分类 —— status 400 且正文命中上下文超限方言
/// → [`RouteFailureClass::ContextOverflow`](从 client 类拆出,见 contracts
/// 的语义变更登记);其余与 [`classify`] 同语义。
pub fn classify_with_body(status: u16, body: Option<&str>) -> RouteFailureClass {
    if status == 400
        && body.is_some_and(crate::gateway::is_context_overflow_body)
    {
        return RouteFailureClass::ContextOverflow;
    }
    classify(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{NO_RETRY_STATUS_CODES, RETRY_STATUS_CODES, should_retry};

    /// C4 断言(主题 #3):400/401/403/404/422 不重试。
    #[test]
    fn no_retry_status() {
        for status in [400u16, 401, 403, 404, 422] {
            assert_eq!(classify(status), RouteFailureClass::Client);
            assert!(!should_retry(status, 0, 3), "{status} 不应重试");
        }
    }

    /// C4 断言:503 触发计划层回退。
    #[test]
    fn fallback_503() {
        assert!(should_fallback_mode(503, "off"));
        assert!(should_fallback_mode(503, "model-chain"));
        assert!(should_fallback_mode(503, "retry"));
    }

    /// C4 断言(主题 #7):501 可重试但**不进熔断判定**——分类为
    /// server(重试/回退参与),而默认熔断 failure_status_codes
    /// (408/429/500/502/503/504)不含 501(wr-infra config 默认值 +
    /// wr-server breaker 仅按该集合计票,见 breaker.rs 测试)。
    #[test]
    fn breaker_ignores_501() {
        assert!(RETRY_STATUS_CODES.contains(&501));
        assert!(should_retry(501, 0, 1));
        assert!(!NO_RETRY_STATUS_CODES.contains(&501));
        let default_failure_codes: Vec<i64> = vec![408, 429, 500, 502, 503, 504];
        assert!(
            !default_failure_codes.contains(&501),
            "熔断默认失败码集合必须不含 501(C4)"
        );
    }

    /// 网络层失败哨兵 0 → retryable(驱动同 provider 重试;
    /// `should_fallback_mode` 对 <400 短路 False,与 Python 一致 ——
    /// 网络失败由 `should_retry(0,..)` 消费,候选链推进是循环自然结果)。
    #[test]
    fn network_sentinel_is_retryable() {
        assert_eq!(classify(0), RouteFailureClass::Retryable);
        assert!(should_retry(0, 0, 3));
        assert!(!should_fallback_mode(0, "off"));
    }

    /// 20260905_03 S7:400 + 上下文超限正文 → ContextOverflow;
    /// 普通 400 仍是 client;非 400 不受正文影响。
    #[test]
    fn classify_with_body_splits_context_overflow() {
        assert_eq!(
            classify_with_body(400, Some("prompt is too long: 10 tokens > 5 maximum")),
            RouteFailureClass::ContextOverflow
        );
        assert_eq!(
            classify_with_body(400, Some("invalid api key")),
            RouteFailureClass::Client
        );
        assert_eq!(classify_with_body(400, None), RouteFailureClass::Client);
        // 超限方言出现在 429 上不触发(仅 400 语义)。
        assert_eq!(
            classify_with_body(429, Some("prompt is too long")),
            RouteFailureClass::RateLimit
        );
    }
}
