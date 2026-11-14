//! 协议无损基座(方案 20261106_01 P1)。
//!
//! 职责边界:
//! - 只做**分帧 + 原始字节 + 共享解析视图**,不执行观察、修正或转换;
//! - SSE / JSON / NDJSON 各自独立分帧,禁止按 Content-Type 子串猜测;
//! - 解析失败或无效 UTF-8 不丢原文,也不伪造成功事件;
//! - 预算超限是硬错误,不做静默截断。
//!
//! **压缩边界**:本模块的 `ByteExact` 是相对于**交给 framer 的实体正文字节**
//! 定义的。HTTP 内容编码(gzip/deflate/br)属于传输层;若传输层解压后
//! 再转发或重新压缩,必须同步声明表示/元数据变化,不能把解压后的原文
//! 冒充压缩线上字节的 ByteExact。
//!
//! 生产链路在 P2 才接入;P1 只固定可复用接口和测试夹具。

pub mod error;
pub mod json;
pub mod ndjson;
pub mod record;
pub mod sse;

use crate::wire::error::WireError;
use crate::wire::record::{RawRecord, Representation};

/// 统一的增量分帧接口。实现必须是任意 chunk 切分不变量:
/// 相同输入字节流在任何切分下产生相同记录序列。
pub trait RecordFramer {
    fn representation(&self) -> Representation;

    /// 喂入一段字节;`received_at_ms` 为调用方时钟(记录首块时间)。
    fn feed(&mut self, chunk: &[u8], received_at_ms: i64) -> Result<Vec<RawRecord>, WireError>;

    /// 输入 EOF;返回尾部记录(可能为空),不得补造终态事件。
    fn finish(&mut self, received_at_ms: i64) -> Result<Vec<RawRecord>, WireError>;
}

impl RecordFramer for sse::SseFramer {
    fn representation(&self) -> Representation {
        Representation::Sse
    }

    fn feed(&mut self, chunk: &[u8], received_at_ms: i64) -> Result<Vec<RawRecord>, WireError> {
        self.feed(chunk, received_at_ms)
    }

    fn finish(&mut self, received_at_ms: i64) -> Result<Vec<RawRecord>, WireError> {
        self.finish(received_at_ms)
    }
}

impl RecordFramer for ndjson::NdjsonFramer {
    fn representation(&self) -> Representation {
        Representation::Ndjson
    }

    fn feed(&mut self, chunk: &[u8], received_at_ms: i64) -> Result<Vec<RawRecord>, WireError> {
        self.feed(chunk, received_at_ms)
    }

    fn finish(&mut self, received_at_ms: i64) -> Result<Vec<RawRecord>, WireError> {
        self.finish(received_at_ms)
    }
}
