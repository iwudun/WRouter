//! 无损分帧错误与资源预算(方案 20261106_01 P1-5)。
//!
//! 预算错误是**硬错误**:分帧器进入粘性失败状态,不再静默截断或跳过
//! 记录。格式错误(JSON 语法、无效 UTF-8)不是分帧错误,保留在
//! [`crate::wire::json::JsonView`] 中,原始字节仍可转发。

use std::fmt;

/// 单个 framer 的资源预算。所有上限都按**未压缩字节**计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireLimits {
    /// 单条记录(含记录分隔符)的最大字节数。
    pub max_record_bytes: usize,
    /// EOF 时未分派尾部残片的最大字节数。
    pub max_tail_bytes: usize,
    /// 整条流的累计字节上限;`None` 表示由传输层另行限流。
    pub max_total_bytes: Option<u64>,
    /// JSON 嵌套深度上限。
    pub max_json_depth: usize,
    /// 重复键/数值风险诊断的最大报告条数(防止诊断本身放大内存)。
    pub max_json_reports: usize,
}

impl Default for WireLimits {
    fn default() -> Self {
        Self {
            max_record_bytes: 16 * 1024 * 1024,
            max_tail_bytes: 1024 * 1024,
            max_total_bytes: None,
            max_json_depth: 64,
            max_json_reports: 64,
        }
    }
}

impl WireLimits {
    pub fn with_max_record_bytes(mut self, bytes: usize) -> Self {
        self.max_record_bytes = bytes;
        self
    }

    pub fn with_max_tail_bytes(mut self, bytes: usize) -> Self {
        self.max_tail_bytes = bytes;
        self
    }

    pub fn with_max_total_bytes(mut self, bytes: u64) -> Self {
        self.max_total_bytes = Some(bytes);
        self
    }

    pub fn with_max_json_depth(mut self, depth: usize) -> Self {
        self.max_json_depth = depth;
        self
    }
}

/// 分帧/记录模型的硬错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// 单条记录超过预算。
    RecordTooLarge { limit: usize, observed: usize },
    /// EOF 残片超过预算。
    TailTooLarge { limit: usize, observed: usize },
    /// 流累计字节超过预算。
    TotalBytesExceeded { limit: u64, observed: u64 },
    /// 记录内容范围非法(仅用于外部构造校验)。
    InvalidRecordRange {
        start: usize,
        end: usize,
        len: usize,
    },
    /// framer 已结束,不能再 feed。
    FramerFinished,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RecordTooLarge { limit, observed } => {
                write!(f, "record exceeds byte budget: {observed} > {limit}")
            }
            Self::TailTooLarge { limit, observed } => {
                write!(
                    f,
                    "trailing record exceeds byte budget: {observed} > {limit}"
                )
            }
            Self::TotalBytesExceeded { limit, observed } => {
                write!(f, "stream exceeds byte budget: {observed} > {limit}")
            }
            Self::InvalidRecordRange { start, end, len } => {
                write!(f, "invalid record range {start}..{end} for {len} bytes")
            }
            Self::FramerFinished => write!(f, "framer already finished"),
        }
    }
}

impl std::error::Error for WireError {}
