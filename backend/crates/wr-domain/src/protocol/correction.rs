//! 原生修正规则注册与归因(方案 20261106_01 P3-2、§8)。
//!
//! 每条规则声明 ID/版本、类别、适用面、profile、读写路径、顺序、预算与
//! 失败策略;执行结果区分原样 / 修改 / 拒绝并可归因。空输出不隐含丢帧。
//!
//! P3 阶段原生路径的活跃修改规则只有 **R8 模型别名**(产品策略):只改
//! profile 指定的协议 model 路径,不递归业务对象。R1/R2/R5/R7 在原生 Raw
//! 路径上的适用部分是**保留 + 观察**而非字节修改 —— 身份/持久化 ID 原样
//! 保留(R2/R7),重放/冲突/终态由 [`crate::semantic::source::SourceObserver`]
//! 的生命周期门记录(R1/R5),合法快照原样透传不重复追加全文(R3)。
//! 跨协议的主动修正/合成在 P4 建立,不在此处预置假想规则。

use serde_json::Value;

use super::rewrite_json_model_fields;

/// 规则类别(§8.1 三类行为分开)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// 规范校验 / 目标编码约束。
    ProtocolValidation,
    /// 已知上游缺陷修复(需 provider/profile 证据)。
    UpstreamDefect,
    /// 产品策略 / 历史修复(显式产品契约,不冒充协议必然)。
    ProductPolicy,
}

/// 规则作用的数据面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionSurface {
    Request,
    Response,
}

impl CorrectionSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}

/// 规则失败时的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePolicy {
    /// 拒绝该记录/请求(提交前 HTTP 错误,提交后协议错误/中断)。
    Reject,
    /// 保留原文不改(修改无法安全落地时不静默改写)。
    KeepOriginal,
}

/// 一条注册规则的静态契约。
#[derive(Debug, Clone, Copy)]
pub struct RuleSpec {
    pub id: &'static str,
    pub version: u32,
    pub kind: RuleKind,
    pub surfaces: &'static [CorrectionSurface],
    /// 适用 profile(空 = 全部原生 profile)。
    pub profiles: &'static [&'static str],
    /// 协议读取路径(诊断用,脱敏定位)。
    pub read_paths: &'static [&'static str],
    /// 允许写入的路径(修改不得越出此范围)。
    pub write_paths: &'static [&'static str],
    /// 组合顺序(小者先;规则不可交换时由规划固定顺序)。
    pub order: u32,
    /// 单记录修改预算(字节;0 = 不限,仅做定位修改)。
    pub budget_bytes: usize,
    pub on_failure: FailurePolicy,
}

/// R8 模型别名:仅协议 model 路径,请求与响应两面共享。
pub const R8_MODEL_ALIAS: RuleSpec = RuleSpec {
    id: "R8-model-alias",
    version: 1,
    kind: RuleKind::ProductPolicy,
    surfaces: &[CorrectionSurface::Request, CorrectionSurface::Response],
    profiles: &[],
    read_paths: &["model", "modelVersion", "message.model", "response.model"],
    write_paths: &["model", "modelVersion", "message.model", "response.model"],
    order: 100,
    budget_bytes: 0,
    on_failure: FailurePolicy::KeepOriginal,
};

/// 原生路径注册表(P3 活跃规则)。顺序按 `order` 升序消费。
pub fn native_registry() -> &'static [RuleSpec] {
    &[R8_MODEL_ALIAS]
}

/// 一次修正的可归因结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrectionOutcome {
    /// 无匹配,原文不变。
    Unchanged,
    /// 已修改,落在声明路径。
    Modified {
        rule: &'static str,
        /// 命中的协议路径(脱敏定位,不含正文)。
        path: &'static str,
    },
    /// 拒绝修改(失败策略 Reject 时升级成协议错误)。
    Rejected { rule: &'static str, reason: String },
}

/// 把模型别名应用到响应/请求 JSON 值,返回是否修改与归因。
///
/// 复用 [`rewrite_json_model_fields`] 的精确路径判定;命中即记 R8 修改,
/// 未命中记原样。调用方据 `changed` 决定是否需要重序列化(未修改保持原字节)。
pub fn apply_model_alias(value: &mut Value, model: &str) -> (bool, CorrectionOutcome) {
    let path = model_alias_path(value);
    let changed = rewrite_json_model_fields(value, model);
    let outcome = match (changed, path) {
        (true, Some(path)) => CorrectionOutcome::Modified {
            rule: R8_MODEL_ALIAS.id,
            path,
        },
        (true, None) => CorrectionOutcome::Modified {
            rule: R8_MODEL_ALIAS.id,
            path: "model",
        },
        (false, _) => CorrectionOutcome::Unchanged,
    };
    (changed, outcome)
}

/// 命中修改的协议路径(用于归因;不暴露业务字段)。
fn model_alias_path(value: &Value) -> Option<&'static str> {
    let map = value.as_object()?;
    if map.get("model").is_some_and(Value::is_string) {
        return Some("model");
    }
    if map.get("modelVersion").is_some_and(Value::is_string) {
        return Some("modelVersion");
    }
    for key in ["message", "response"] {
        if let Some(nested) = map.get(key).and_then(Value::as_object)
            && nested.get("model").is_some_and(Value::is_string)
        {
            return Some(if key == "message" {
                "message.model"
            } else {
                "response.model"
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_declares_required_metadata() {
        let registry = native_registry();
        assert!(!registry.is_empty());
        for rule in registry {
            assert!(!rule.id.is_empty());
            assert!(rule.version >= 1);
            assert!(!rule.write_paths.is_empty());
            assert!(!rule.surfaces.is_empty());
        }
        let r8 = &registry[0];
        assert_eq!(r8.id, "R8-model-alias");
        assert_eq!(r8.kind, RuleKind::ProductPolicy);
        assert_eq!(r8.on_failure, FailurePolicy::KeepOriginal);
    }

    #[test]
    fn alias_modifies_only_protocol_path_and_attributes() {
        let mut value = json!({
            "model": "upstream-model",
            "metadata": {"model": "business"},
            "choices": [{"message": {"tool_calls": [{"function": {"arguments": "{\"model\":\"x\"}"}}]}}],
        });
        let (changed, outcome) = apply_model_alias(&mut value, "aggregate");
        assert!(changed);
        assert_eq!(
            outcome,
            CorrectionOutcome::Modified {
                rule: "R8-model-alias",
                path: "model",
            }
        );
        assert_eq!(value["model"], "aggregate");
        assert_eq!(value["metadata"]["model"], "business");
    }

    #[test]
    fn alias_no_match_is_unchanged() {
        let mut value = json!({"metadata": {"model": "business"}});
        let (changed, outcome) = apply_model_alias(&mut value, "aggregate");
        assert!(!changed);
        assert_eq!(outcome, CorrectionOutcome::Unchanged);
        assert_eq!(value["metadata"]["model"], "business");
    }

    #[test]
    fn alias_idempotent() {
        let mut value = json!({"response": {"model": "m"}});
        let (first, _) = apply_model_alias(&mut value, "aggregate");
        let (second, outcome) = apply_model_alias(&mut value, "aggregate");
        assert!(first);
        assert!(!second);
        assert_eq!(outcome, CorrectionOutcome::Unchanged);
        assert_eq!(value["response"]["model"], "aggregate");
    }
}
