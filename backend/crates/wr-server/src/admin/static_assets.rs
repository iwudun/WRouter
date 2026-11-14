//! 管理面静态资源托管(方案 20261102_01 P1-1)。
//!
//! 目标:让 `wr-backend` 单进程同时扮演 API 服务与前端静态站点 ——
//! 浏览器 / Tauri webview 直接打开 `http://127.0.0.1:3462/` 即为完整
//! 管理面,无需 Electron 壳。
//!
//! **零新增依赖(硬约束)**:后端长期按「离线可解」构建
//! (`backend/Cargo.toml` 依赖注释),故不引入 `tower-http` 的 `fs`
//! 特性(需 `http-range-header` / `mime_guess`,均不在 Cargo.lock 中)
//! 也不用 `mime_guess` —— 静态托管全部用 `std::fs` 手写实现,
//! `Cargo.toml` **零改动**。
//!
//! 安全要点:
//! - **路径穿越**:URL 路径经百分比解码后逐段过滤(`..` / `:` / `\`),
//!   且最终文件必须 `canonicalize` 后仍位于 `frontend_dir` 之内;
//! - **API 404 不被吞掉**:`/api/*` 前缀不参与 SPA fallback
//!   (R7 登记风险,分流在 `super::ui_fallback` 完成);
//! - **前端目录缺失不 panic**:`frontend_dir` 不存在或不是目录 → 404,
//!   网关面与 API 完全不受影响。

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;

use super::AdminState;

/// SPA 入口文件名(vite 构建产物固定名)。
const INDEX_FILE: &str = "index.html";

/// MIME 表(手写,覆盖 vite 产出的常见扩展名;未命中 → octet-stream)。
fn mime_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// 缓存策略:入口页不缓存(vite 产物文件名带 hash,`/assets/` 可长期缓存)。
fn cache_control(file_name: &str) -> &'static str {
    if file_name == INDEX_FILE {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    }
}

/// 十六进制字符 → 数值。
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 百分比解码(手写,避免引入 `percent-encoding` 依赖)。
///
/// `%XX` → 字节;非法转义 / 非 UTF-8 → None(调用方按未命中处理)。
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            out.push((high << 4) | low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 把 URL 路径解析为 `root` 内的真实文件路径。
///
/// 返回 `None` = 未命中(文件不存在 / 越界 / 路径非法)→ 调用方走
/// SPA fallback。**双重防护**:逐段过滤 + `canonicalize` 后仍在根内。
fn resolve_within(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path)?;
    if decoded.contains('\0') {
        return None;
    }
    // 根目录不存在 → 整体未命中(调用方回 404)。
    let root_real = root.canonicalize().ok()?;
    let mut candidate = root_real.clone();
    for segment in decoded.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => continue,
            // 拒绝上跳:不依赖 canonicalize 兜底,显式早退。
            ".." => return None,
            // `:`(盘符/ADS)与 `\`(Windows 分隔符)一律拒绝。
            other if other.contains(':') || other.contains('\\') => return None,
            other => candidate.push(other),
        }
    }
    let real = candidate.canonicalize().ok()?;
    // 越界检查(Path::starts_with 按路径分量比较,不会被同前缀目录骗过)。
    if !real.starts_with(&root_real) {
        return None;
    }
    if !real.is_file() {
        return None;
    }
    Some(real)
}

/// 读文件(阻塞 IO 丢进 `spawn_blocking`,不占用 async worker)。
async fn read_file(path: PathBuf) -> std::io::Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(std::io::Error::other)?
}

/// 构造文件响应(MIME + 缓存头)。
fn file_response(path: &Path, bytes: Vec<u8>) -> Response {
    let mime = mime_for(path);
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let cache = cache_control(file_name);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(mime))
        .header(header::CACHE_CONTROL, HeaderValue::from_static(cache))
        .body(Body::from(bytes))
        .unwrap_or_else(|_| super::not_found_response())
}

/// 静态托管主入口。
///
/// 命中文件 → 返回文件;未命中 → SPA fallback(返回 `index.html`);
/// 前端目录缺失 / 入口缺失 → 404 JSON(不 panic,网关面不受影响)。
pub async fn serve_static(state: &AdminState, url_path: &str) -> Response {
    let root = state.settings.frontend_dir.clone();
    if !root.is_dir() {
        tracing::warn!(
            target: "wrouter.admin",
            dir = %root.display(),
            "static_assets_dir_missing"
        );
        return super::not_found_response();
    }

    if let Some(file) = resolve_within(&root, url_path) {
        match read_file(file.clone()).await {
            Ok(bytes) => return file_response(&file, bytes),
            Err(e) => {
                tracing::warn!(
                    target: "wrouter.admin",
                    path = %file.display(),
                    %e,
                    "static_asset_read_failed"
                );
                return super::not_found_response();
            }
        }
    }

    // SPA fallback:未命中且非 `/api/` 前缀 → 入口页。
    let index = root.join(INDEX_FILE);
    if !index.is_file() {
        return super::not_found_response();
    }
    match read_file(index.clone()).await {
        Ok(bytes) => file_response(&index, bytes),
        Err(e) => {
            tracing::warn!(
                target: "wrouter.admin",
                path = %index.display(),
                %e,
                "static_index_read_failed"
            );
            super::not_found_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_escapes_and_rejects_garbage() {
        assert_eq!(percent_decode("/a/b.js").as_deref(), Some("/a/b.js"));
        assert_eq!(percent_decode("/a%20b.js").as_deref(), Some("/a b.js"));
        assert_eq!(percent_decode("/a%2Fb.js").as_deref(), Some("/a/b.js"));
        assert_eq!(percent_decode("/a%zz.js"), None);
        assert_eq!(percent_decode("/a%2"), None);
    }

    #[test]
    fn mime_table_covers_vite_assets() {
        assert_eq!(
            mime_for(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_for(Path::new("assets/app-a1b2.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_for(Path::new("assets/app.css")), "text/css; charset=utf-8");
        assert_eq!(
            mime_for(Path::new("assets/x.woff2")),
            "font/woff2"
        );
        assert_eq!(mime_for(Path::new("assets/x.unknown")), "application/octet-stream");
    }
}
