//! R4.8 前置限流 / 预算测试(`wr-server::gateway::limits` 公开面)。
//!
//! 逐条钉 Python `application/gateway/precheck.py` 的语义:固定窗口、
//! 双作用域、指标口径、预算回滚、limits 翻译、身份截断。

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};

use wr_server::gateway::limits::{
    PrecheckConfig, PrecheckEstimate, PrecheckFailure, PrecheckResult, Prechecker,
    api_key_limits_to_rules, extract_identity,
};

struct Clock(AtomicI64);

impl Clock {
    fn new(start: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(start)))
    }
    fn set(&self, value: i64) {
        self.0.store(value, Ordering::SeqCst);
    }
}

fn build_checker(clock: &Arc<Clock>, config: PrecheckConfig) -> Prechecker {
    let inner = clock.clone();
    Prechecker::new(config).with_clock(move || inner.0.load(Ordering::SeqCst))
}

fn rules(metrics: &[(&str, i64)]) -> Vec<Value> {
    metrics
        .iter()
        .map(|(metric, limit)| json!({"metric": metric, "limit": limit}))
        .collect()
}

fn config_with(enabled: bool, rate_rules: Vec<Value>, budget: f64) -> PrecheckConfig {
    PrecheckConfig {
        enabled,
        rate_limit_rules: rate_rules,
        budget_max_cost_usd: budget,
    }
}

fn rejected(result: PrecheckResult) -> PrecheckFailure {
    match result {
        PrecheckResult::Rejected(failure) => failure,
        PrecheckResult::Ok(_) => panic!("预期被拒"),
    }
}

fn assert_ok(result: PrecheckResult) {
    assert!(matches!(result, PrecheckResult::Ok(_)), "预期放行,实际被拒");
}

#[test]
fn rpm_limit_rejects_with_429_after_threshold() {
    let clock = Clock::new(60_000);
    let checker = build_checker(&clock, config_with(true, rules(&[("rpm", 2)]), 0.0));
    assert_ok(checker.evaluate("id", None, 0.0, &[]));
    assert_ok(checker.evaluate("id", None, 0.0, &[]));
    let failure = rejected(checker.evaluate("id", None, 0.0, &[]));
    assert_eq!(failure.status, 429);
    assert_eq!(failure.code, "rate_limit_exceeded");
    assert_eq!(failure.message, "Rate limit exceeded for RPM (3/2).");
    assert_eq!(failure.details["scope"], json!("global"));
    assert_eq!(failure.details["metric"], json!("rpm"));
    assert_eq!(failure.details["used"], json!(3));
    assert_eq!(failure.details["limit"], json!(2));
    // 客户端错误体:type 与 code 同值(镜像 forwarding.py 的拼装)。
    assert_eq!(
        failure.to_body()["error"]["type"],
        json!("rate_limit_exceeded")
    );
    assert_eq!(
        failure.to_body()["error"]["code"],
        json!("rate_limit_exceeded")
    );
}

#[test]
fn window_rollover_resets_counters() {
    let clock = Clock::new(0);
    let checker = build_checker(&clock, config_with(true, rules(&[("rpm", 1)]), 0.0));
    assert_ok(checker.evaluate("id", None, 0.0, &[]));
    assert!(matches!(
        checker.evaluate("id", None, 0.0, &[]),
        PrecheckResult::Rejected(_)
    ));
    clock.set(60_000);
    assert!(
        matches!(
            checker.evaluate("id", None, 0.0, &[]),
            PrecheckResult::Ok(_)
        ),
        "固定窗口在窗口起点换格后从 0 重新计数"
    );
}

#[test]
fn identities_and_metrics_are_isolated() {
    let clock = Clock::new(1_000);
    let checker = build_checker(
        &clock,
        config_with(true, rules(&[("rpm", 1), ("tpm", 50)]), 0.0),
    );
    assert_ok(checker.evaluate("a", None, 0.0, &[]));
    assert_ok(checker.evaluate("b", None, 0.0, &[]));
    assert!(matches!(
        checker.evaluate("a", None, 0.0, &[]),
        PrecheckResult::Rejected(_)
    ));
    // token 指标按自己的桶计:同一身份第二次把 40+40=80 堆到 tpm 上。
    let token_only = build_checker(&clock, config_with(true, rules(&[("tpm", 50)]), 0.0));
    let estimate = PrecheckEstimate {
        total_tokens: 40,
        ..Default::default()
    };
    assert_ok(token_only.evaluate("c", Some(estimate.clone()), 0.0, &[]));
    let failure = rejected(token_only.evaluate("c", Some(estimate), 0.0, &[]));
    assert_eq!(failure.details["metric"], json!("tpm"));
    assert_eq!(failure.details["used"], json!(80));
}

/// per-key 规则不受全局 `enabled` 门控,且与全局计数分桶。
#[test]
fn key_scope_ignores_global_enabled_flag() {
    let clock = Clock::new(1_000);
    let checker = build_checker(&clock, config_with(false, rules(&[("rpm", 100)]), 0.0));
    assert_ok(checker.evaluate("id", None, 0.0, &rules(&[("rpm", 1)])));
    let failure = rejected(checker.evaluate("id", None, 0.0, &rules(&[("rpm", 1)])));
    assert_eq!(failure.details["scope"], json!("key"));
    // 全局 rpm=100 的桶与 key 作用域桶互不占用。
    for _ in 0..5 {
        assert_ok(checker.evaluate("id", None, 0.0, &[]));
    }
}

#[test]
fn zero_amount_metrics_are_skipped() {
    let clock = Clock::new(1_000);
    let checker = build_checker(&clock, config_with(true, rules(&[("ipm", 1)]), 0.0));
    for _ in 0..5 {
        assert_ok(checker.evaluate("id", Some(PrecheckEstimate::default()), 0.0, &[]));
    }
    // 一旦估算出图片数,ipm 桶就开始计费(limit=1 → 第二次越限)。
    let with_images = PrecheckEstimate {
        image_count: 1,
        ..Default::default()
    };
    assert_ok(checker.evaluate("img", Some(with_images.clone()), 0.0, &[]));
    assert!(matches!(
        checker.evaluate("img", Some(with_images), 0.0, &[]),
        PrecheckResult::Rejected(_)
    ));
}

#[test]
fn unknown_metrics_and_non_positive_limits_are_ignored() {
    let clock = Clock::new(1_000);
    let mut rate_rules = rules(&[("tps", 1), ("rpm", 0)]);
    rate_rules.push(json!("not-an-object"));
    rate_rules.push(json!({"limit": 1}));
    let checker = build_checker(&clock, config_with(true, rate_rules, 0.0));
    for _ in 0..3 {
        assert_ok(checker.evaluate("id", None, 0.0, &[]));
    }
}

/// 预算:超线拒、回滚累加值(重试不会叠加),并给出 used/requested。
#[test]
fn budget_rejects_and_rolls_back_speculative_add() {
    let clock = Clock::new(1_000);
    let checker = build_checker(&clock, config_with(true, vec![], 1.0));
    assert_ok(checker.evaluate("id", None, 0.6, &[]));
    let failure = rejected(checker.evaluate("id", None, 0.6, &[]));
    assert_eq!(failure.status, 402);
    assert_eq!(failure.code, "budget_exceeded");
    assert_eq!(failure.details["used"], json!(0.6));
    assert_eq!(failure.details["requested"], json!(0.6));
    assert_eq!(failure.details["limit"], json!(1.0));
    let second = rejected(checker.evaluate("id", None, 0.6, &[]));
    assert_eq!(
        second.details["used"],
        json!(0.6),
        "回滚后累计仍是 0.6,不会变成 1.8"
    );
}

#[test]
fn budget_falls_back_to_estimate_when_requested_is_zero() {
    let clock = Clock::new(1_000);
    let checker = build_checker(&clock, config_with(true, vec![], 1.0));
    let estimate = PrecheckEstimate {
        estimated_cost_usd: 2.0,
        ..Default::default()
    };
    assert!(matches!(
        checker.evaluate("id", Some(estimate), 0.0, &[]),
        PrecheckResult::Rejected(_)
    ));
}

#[test]
fn budget_disabled_by_zero_never_charges() {
    let clock = Clock::new(1_000);
    let checker = build_checker(&clock, config_with(true, vec![], 0.0));
    for _ in 0..5 {
        assert_ok(checker.evaluate("id", None, 100.0, &[]));
    }
}

#[test]
fn api_key_limits_translation() {
    let limits = json!({
        "rpm": 30, "tpd": 100_000, "maxRequests": 5, "windowMs": 10_000,
        "maxTokens": 900, "quotaWindowMs": 0, "ipm": 9,
    });
    assert_eq!(
        api_key_limits_to_rules(Some(&limits)),
        vec![
            json!({"metric": "rpm", "limit": 30}),
            json!({"metric": "tpd", "limit": 100_000}),
            json!({"metric": "rpm", "limit": 5, "window_ms": 10_000}),
            json!({"metric": "tpm", "limit": 900, "window_ms": 86_400_000}),
        ],
        "ipm 不翻译(估算器产不出 image_count);window 缺省回落默认值"
    );
    assert!(api_key_limits_to_rules(None).is_empty());
    assert!(api_key_limits_to_rules(Some(&json!("nope"))).is_empty());
    assert!(api_key_limits_to_rules(Some(&json!({"rpm": 0}))).is_empty());
}

#[test]
fn identity_truncates_credential_to_12_chars() {
    let headers = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    };
    assert_eq!(
        extract_identity(&headers(&[("x-api-key", "0123456789abcdefghij")])),
        "key:0123456789ab"
    );
    assert_eq!(
        extract_identity(&headers(&[("X-Api-Key", "short")])),
        "key:short",
        "header 名大小写不敏感"
    );
    assert_eq!(
        extract_identity(&headers(&[("authorization", "Bearer sk-abcdefghijklmnop")])),
        "key:sk-abcdefghi",
        "bearer 从第 7 个字符起取 12 位"
    );
    assert_eq!(extract_identity(&headers(&[("api-key", "k")])), "key:k");
    assert_eq!(extract_identity(&headers(&[])), "anonymous");
    assert_eq!(
        extract_identity(&headers(&[("authorization", "Basic xx")])),
        "anonymous",
        "非 Bearer 方案不参与身份"
    );
}

#[test]
fn update_config_takes_effect_immediately() {
    let clock = Clock::new(1_000);
    let checker = build_checker(&clock, config_with(false, vec![], 0.0));
    assert_ok(checker.evaluate("id", None, 0.0, &[]));
    checker.update_config(&config_with(true, rules(&[("rpm", 0)]), 0.0));
    // limit<=0 的规则仍被跳过 → 依旧放行。
    assert_ok(checker.evaluate("id", None, 0.0, &[]));
    checker.update_config(&config_with(true, rules(&[("rpm", 1)]), 0.0));
    assert_ok(checker.evaluate("fresh", None, 0.0, &[]));
    assert!(matches!(
        checker.evaluate("fresh", None, 0.0, &[]),
        PrecheckResult::Rejected(_)
    ));
}
