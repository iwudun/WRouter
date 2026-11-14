//! 路由领域模型(镜像 Python `domain/routing/`,R3)。
//!
//! 文件与 Python 同名对应(14 号文档 R3「镜像时效性声明」:迁移期
//! 1:1 映射供评审对照,parity 稳定后允许按 Rust 惯例重组):
//! - [`contracts`] — 路由 DTO(`contracts.py`);
//! - [`model_registry`] — 模型注册与解析(`model_registry.py`;计划中的
//!   `model_resolution.rs` 在 Python 侧与之同文件,合并落点);
//! - [`config_compiler`] — 配置编译(`compiler.py`;计划名
//!   `config_compiler.rs`);
//! - [`execution_plan`] — 执行计划(`execution_plan.py`);
//! - [`failure_classifier`] — 失败分类(`failure_classifier.py`;
//!   实现 re-export 自 [`crate::gateway`] 单一事实源);
//! - [`protocol_adapter`] — Gemini path-style 改写(`protocol_adapter.py`;
//!   纯函数 + 测试钉住,网关层直接改写 model,不接线,R3.11);
//!
//! 行为锚(C4):计划层 → 候选链层 → 同 provider 层三层回退;
//! 流式无原地重试在 `wr-server::gateway::forward` 断言;熔断判定
//! 不含 501 在 [`failure_classifier`] 命名测试。

pub mod config_compiler;
pub mod contracts;
pub mod execution_plan;
pub mod failure_classifier;
pub mod model_registry;
pub mod protocol_adapter;
