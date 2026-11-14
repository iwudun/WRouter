//! 环境装配(镜像 Python `settings.py::Settings.from_env`)。
//!
//! 环境变量名、缺省值、解析失败回退行为与 Python 版逐条对齐
//! (C1 端口分配:3460 网关 / 3462 管理 / 3463 代理;3461 迁移兼容口
//! 已随迁移结束移除,3463 仍为状态字段占位、无监听)。

use std::path::{Path, PathBuf};

use crate::service_control::ManagedBy;

/// Python `AUTH_TOKEN_FILENAME`:管理令牌按数据目录持久化,
/// 保证后端进程崩溃自动重启后令牌不变(否则渲染进程永久 401)。
pub const AUTH_TOKEN_FILENAME: &str = "auth_token";

/// 入站请求体上限默认值(20261016_01 A-4):64MiB —— 覆盖大上下文与
/// base64 多模态请求,同时挡住无界的 body 缓冲。
pub const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Settings {
    pub app_name: String,
    pub version: String,
    pub host: String,
    pub gateway_port: u16,
    pub web_port: u16,
    pub proxy_port: u16,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub auth_token: String,
    pub parent_pid: Option<u32>,
    pub ready_file: Option<PathBuf>,
    /// 正文保留天数(R1 仅承载;过期清理在 R4)。0 = 永久。
    pub body_retention_days: i64,
    /// 元数据保留天数。0 = 永久。
    pub log_retention_days: i64,
    /// 单条正文捕获上限字节数(`WROUTER_BODY_CAPTURE_MAX_BYTES`,默认 512KB)。
    pub body_capture_max_bytes: usize,
    /// 入站请求体上限字节数(`WROUTER_MAX_BODY_BYTES`,默认 64MiB)。
    /// 20261016_01 A-4:此前对齐 Python/uvicorn 无上限(`DefaultBodyLimit::
    /// disable`),超大请求会同时缓冲原文 + JSON Value + 转发克隆;现在在
    /// 读取阶段即以 413 拒绝。改配置需重启(层在路由构建时静态挂载)。
    pub max_body_bytes: usize,
    /// capabilities 契约文件路径(默认 `<repo_root>/contracts/wrouter-capabilities.json`)。
    pub capabilities_file: PathBuf,
    /// 前端 SPA dist 目录(默认 `<repo_root>/frontend/dist`;不存在时网关仍工作)。
    pub frontend_dir: PathBuf,
    /// 管理面是否同时托管前端 SPA(`WROUTER_SERVE_UI`,默认 false)。
    ///
    /// 20261102_01 P1-5:置位后管理面额外提供静态资源路由与
    /// `GET /api/wr/session`(浏览器 / Tauri 首屏取令牌);**缺省 false
    /// 时行为与改造前完全一致**(回归验收项)。
    pub serve_ui: bool,
    /// 后端的托管方式(20261102_01 P6):决定页面「退出服务」的语义。
    ///
    /// - `embedded`:桌面壳同进程(Tauri 壳置 `WROUTER_MANAGED_BY=embedded`);
    /// - `systemd`:systemd 托管(`INVOCATION_ID` 探测);
    /// - `process`:其余(独立进程)。
    pub managed_by: ManagedBy,
}

impl Settings {
    pub fn from_env() -> Self {
        let root = std::env::var_os("WROUTER_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".wrouter"));
        let config_dir = std::env::var_os("WROUTER_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("config"));
        let auth_token = resolve_auth_token(&root);
        let parent_pid = std::env::var("WROUTER_PARENT_PID")
            .ok()
            .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            .and_then(|p| p.parse().ok());
        Self {
            app_name: "WRouter".to_owned(),
            version: super::VERSION.to_owned(),
            host: std::env::var("WROUTER_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
            gateway_port: int_env("WROUTER_GATEWAY_PORT", 3460),
            web_port: int_env("WROUTER_WEB_PORT", 3462),
            proxy_port: int_env("WROUTER_PROXY_PORT", 3463),
            data_dir: root,
            config_dir,
            auth_token,
            parent_pid,
            ready_file: path_env("WROUTER_READY_FILE"),
            body_retention_days: int_env("WROUTER_BODY_RETENTION_DAYS", 7),
            log_retention_days: int_env("WROUTER_LOG_RETENTION_DAYS", 0),
            // 正文捕获上限(Python `WROUTER_BODY_CAPTURE_MAX_BYTES`,默认 512KB)。
            body_capture_max_bytes: (int_env(
                "WROUTER_BODY_CAPTURE_MAX_BYTES",
                wr_domain::observability::RAW_BODY_MAX_BYTES as i64,
            )
            .max(1)) as usize,
            // 入站请求体上限(20261016_01 A-4;0 视为未设置走默认)。
            max_body_bytes: (int_env(
                "WROUTER_MAX_BODY_BYTES",
                DEFAULT_MAX_BODY_BYTES as i64,
            )
            .max(0)) as usize,
            // capabilities / frontend 路径:env 优先,否则基于 CARGO_MANIFEST_DIR 推算。
            capabilities_file: resolve_path_env_or(
                "WROUTER_CAPABILITIES_FILE",
                repo_root()
                    .join("contracts")
                    .join("wrouter-capabilities.json"),
            ),
            frontend_dir: resolve_path_env_or(
                "WROUTER_FRONTEND_DIR",
                repo_root().join("frontend").join("dist"),
            ),
            // 20261102_01 P1-5:管理面静态托管开关(默认关闭 → 行为不变)。
            serve_ui: bool_env("WROUTER_SERVE_UI", false),
            // 20261102_01 P6:托管方式(embedded / process / systemd)。
            managed_by: ManagedBy::from_env(),
        }
    }

    pub fn ensure_directories(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.config_dir)
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn path_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Python `_int_env`:解析失败静默回退默认值。
fn int_env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// 布尔 env 解析(20261102_01 P1-5):`1/true/yes/on` 为真,
/// `0/false/no/off` 为假(大小写不敏感);其余值静默回落缺省。
fn bool_env(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

/// 可选路径 env 优先;未设置时回落 `fallback`。
fn resolve_path_env_or(name: &str, fallback: PathBuf) -> PathBuf {
    path_env(name).unwrap_or(fallback)
}

/// 仓库根:编译期注入(CARGO_MANIFEST_DIR 是 cargo 编译期变量,运行时
/// env 里不存在)—— wr-server → crates → backend → 仓库根,与 Python
/// settings.py 的 parents[3] 同层;contracts/ 与 frontend/ 都挂在仓库根下。
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Python `_resolve_auth_token`:WROUTER_WEB_AUTH_TOKEN 覆盖 →
/// 持久化 `<data_dir>/auth_token` → 新生成并持久化。
///
/// R1 的随机令牌用 UUID v4(熵 122bit);Python 为 token_urlsafe(32)。
/// 令牌值本身无格式契约,仅要求随机与稳定,R6 交付前统一复核。
fn resolve_auth_token(data_dir: &Path) -> String {
    if let Ok(env_token) = std::env::var("WROUTER_WEB_AUTH_TOKEN")
        && !env_token.is_empty()
    {
        return env_token;
    }
    let token_file = data_dir.join(AUTH_TOKEN_FILENAME);
    if let Ok(persisted) = std::fs::read_to_string(&token_file) {
        let trimmed = persisted.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(&token_file, token.clone() + "\n");
    token
}

#[cfg(test)]
mod tests {
    use super::int_env;

    #[test]
    fn int_env_falls_back_like_python() {
        // Safety:测试单线程节点,环境变量无并发访问。
        unsafe {
            std::env::set_var("WR_TEST_PORT_OK", "12345");
            std::env::set_var("WR_TEST_PORT_BAD", "not-a-number");
        }
        assert_eq!(int_env("WR_TEST_PORT_OK", 3462), 12345);
        assert_eq!(int_env("WR_TEST_PORT_BAD", 3462), 3462);
        assert_eq!(int_env("WR_TEST_PORT_MISSING", 3462), 3462);
        unsafe {
            std::env::remove_var("WR_TEST_PORT_OK");
            std::env::remove_var("WR_TEST_PORT_BAD");
        }
    }
}
