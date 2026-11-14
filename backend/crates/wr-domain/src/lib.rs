//! WRouter 领域层(方案 #14 D14)。
//!
//! 分层镜像自原 Python 后端(`backend/src/wrouter/domain/`,R7 退役,
//! 目录名由本 workspace 接替):`configuration` / `gateway` / `observability` /
//! `policy` / `protocol` / `provider` / `proxy` / `routing`。
//!
//! 纯度守护(方案 #14 R1.2 / §5.2):本 crate 不得引入 async 运行时、
//! Web 框架、HTTP 客户端或 SQLite 驱动;`scripts/check-rust.sh` 会在
//! 质量门中 grep 本 crate 的 Cargo.toml 强制执行(对应 Python 版
//! domain/ 不导入 FastAPI/sqlite3/httpx/pydantic 的既有纪律)。
//!
//! R1 落地最小纯逻辑(`policy::PolicyEffect`、
//! `configuration::normalize_theme`)。R2 落地数据面纯逻辑:
//! - [`python_semantics`]:Python 值语义助手(自 wr-infra 下沉,
//!   wr-infra 保留同名 re-export);
//! - [`json_py`]:Python `json.dumps` 等价序列化(relay 重序列化依赖);
//! - [`sse`]:SSE 解析/编码(`application/sse/parser.py` 镜像);
//! - [`provider`]:Provider 实体与解析(错误路径镜像 ValueError);
//! - [`gateway`]:重试/回退策略与 provider 选择;
//! - `protocol`:端点构造、上游头、错误信封、协议嗅探(`crate::protocol`);
//! - [`relay`]:流式 relay(openai 重序列化 / anthropic 透传 / gemini);
//! - `usage`:用量归一化(`crate::usage`)。
//!
//! R3 迁入(路由引擎与协议适配):
//! - [`routing`]:contracts/编译/策略链/执行计划/失败分类/Gemini 路径
//!   适配器(镜像 `domain/routing/`,与 Python 同名文件一一对应);
//! - [`policy`] 全量:RBAC 访问控制引擎(镜像 `domain/policy/`);
//! - `protocol::{convert, gemini, responses, synthesis}`:跨协议
//!   请求/响应转换、Gemini payload 重建、Responses 信封、A6 流合成;
//! - [`relay`] 全矩阵:跨协议流式转换(含 Responses 客户端 relay)。
//!
//! `observability`/`proxy` 仍为占位(R4 迁入)。
//!
//! R6 迁入(模型指纹检测,方案 20261030_01 阶段一):
//! - [`fingerprint`]:模型指纹检测(纯计算:特征/线性代数/探针套件/建库/
//!   打分与证据强度分级);与既有的「客户端指纹」(`protocol::client_fingerprint`)
//!   是两回事,命名全程隔离。

pub mod cache_prefix;
pub mod compression;
pub mod configuration;
pub mod digest;
pub mod fingerprint;
pub mod gateway;
pub mod json_py;
pub mod observability;
pub mod policy;
pub mod protocol;
pub mod provider;
pub mod proxy;
pub mod python_semantics;
pub mod rest;
pub mod routing;
pub mod semantic;
pub mod sse;
pub mod usage;
pub mod wire;
