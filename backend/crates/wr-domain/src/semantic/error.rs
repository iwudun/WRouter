//! 统一错误类别(B7;方案 §3.5)。客户端信封由 Inbound 侧编码器产出,
//! 形态与旧 `format_error_for_protocol` 五种信封逐字段一致(见
//! [`crate::semantic::envelope`])。
//!
//! 与 RFC 草案的差异:`legacy_type` 为落码新增 —— 客户端信封需要上游
//! 归一 error_type 原值(wire 一致性),`category` 是语义归类,二者并存
//! (raw 与 semantic 并存原则的同一应用)。

use serde::Deserialize;
use serde::Serialize;

use crate::protocol::UpstreamError;

/// 统一内部错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Auth,
    RateLimit,
    InvalidRequest,
    /// 组合/能力不支持(矩阵 unsupported 格)。
    Unsupported,
    UpstreamService,
    Transport,
    StreamIncomplete,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryHint {
    Retryable {
        /// 建议等待毫秒(retry-after 解析结果;未知为 None)。
        after_ms: Option<u64>,
    },
    NotRetryable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticError {
    pub category: ErrorCategory,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub param: Option<String>,
    pub message: String,
    pub retryable: RetryHint,
    /// 上游归一 error_type 原值(信封编码保真所需;缺省按状态码推导)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_type: Option<String>,
    /// 上游 body 摘要(已脱敏;采集层负责完整脱敏,内核仅截断)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_excerpt: Option<String>,
}

impl SemanticError {
    /// 组合不支持(矩阵格目标策略;经客户端信封升格 wrouter_error 形态)。
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            category: ErrorCategory::Unsupported,
            status: None,
            code: Some("unsupported_protocol_combination".to_owned()),
            param: None,
            message: message.into(),
            retryable: RetryHint::NotRetryable,
            legacy_type: None,
            raw_excerpt: None,
        }
    }

    /// 旧 `classify_upstream_error` 的归一化结果 → 语义错误。
    pub fn from_upstream(error: &UpstreamError) -> Self {
        let category = if error.error_type.contains("auth")
            || error.error_type.contains("permission")
            || error.status == 401
            || error.status == 403
        {
            ErrorCategory::Auth
        } else if error.error_type.contains("rate_limit") || error.status == 429 {
            ErrorCategory::RateLimit
        } else if error.status >= 500 {
            ErrorCategory::UpstreamService
        } else if error.status >= 400 {
            ErrorCategory::InvalidRequest
        } else {
            ErrorCategory::Internal
        };
        let retryable = if error.status == 429 || error.status >= 500 {
            RetryHint::Retryable { after_ms: None }
        } else {
            RetryHint::NotRetryable
        };
        Self {
            category,
            status: Some(error.status),
            code: error.code.clone(),
            param: error.param.clone(),
            message: error.message.clone(),
            retryable,
            legacy_type: Some(error.error_type.clone()),
            raw_excerpt: None,
        }
    }

    /// 信封编码所需的归一 error_type。
    pub fn effective_type(&self) -> String {
        self.legacy_type
            .clone()
            .unwrap_or_else(|| match self.status {
                Some(status) => default_type_for(status),
                None => "wrouter_error".to_owned(),
            })
    }

    /// 信封编码所需的状态码(缺省 502,镜像 wrouter_error 升格)。
    pub fn effective_status(&self) -> u16 {
        self.status.unwrap_or(502)
    }
}

/// `status_type` 的语义层镜像(状态码 → 默认归一 type)。
pub fn default_type_for(status: u16) -> String {
    crate::protocol::status_type(status)
        .unwrap_or(if status >= 500 {
            "upstream_error"
        } else {
            "request_error"
        })
        .to_owned()
}
