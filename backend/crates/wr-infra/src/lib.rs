//! WRouter 基础设施层(方案 #14 D14)。
//!
//! 依赖方向:`wr-infra → {wr-app(端口 trait), wr-domain}`。本层承载
//! 一切与外部系统的适配:配置 JSON 读取(C9 兼容)、SQLite 持久化
//! (R4,直接读写现有 db 文件,C5 零迁移)、上游 HTTP 客户端(R2)。
//!
//! R1 落地 [`config`]:config JSON 读取,与 Python
//! `domain/configuration/entities.py::AppConfig.from_dict` 逐行为对齐
//! (键别名、缺省值、钳制、truthy 强转)。
//!
//! R2 起新增:sqlite 配置源(只读)、上游 reqwest 客户端、出站代理
//! 决策。`python_semantics` 已下沉到 wr-domain(领域层也需要 Python
//! 值语义,如 Provider 解析),此处保留同名 re-export 以免既有
//! `use crate::python_semantics::…` 引用断裂。

pub mod ca;
pub mod config;
pub mod proxy;
pub mod sqlite;
pub mod sqlite_config;
pub mod upstream;

pub mod python_semantics {
    pub use wr_domain::python_semantics::*;
}
