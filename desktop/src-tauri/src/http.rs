//! 极简 HTTP/1.1 客户端(零第三方依赖)。
//!
//! 桌面壳只需要访问**本机回环**上的管理面(固定 `http://127.0.0.1:3462`),
//! 不需要 TLS —— 因此不引入 reqwest / tauri-plugin-http,手写一个够用的
//! POST + 响应解析(含 chunked 解码)即可。
//!
//! 与后端「零新增 Cargo 依赖」的约束同源:桌面壳自己也是独立工程,
//! 能不引就不引。

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 发一次 `POST` 并返回响应体文本。
///
/// - `headers`:额外请求头(如 `x-wrouter-web-auth`)。
/// - 固定 `Connection: close` → 读完即 EOF,不需要处理 keep-alive。
pub async fn post(
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<String, String> {
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n"
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!(
        "Content-Length: {}\r\n\r\n",
        body.as_bytes().len()
    ));
    request.push_str(body);

    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|error| format!("连接管理面 127.0.0.1:{port} 失败: {error}"))?;
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("写请求失败: {error}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|error| format!("读响应失败: {error}"))?;

    let (head, payload) = split_head_and_body(&raw)?;
    let status_ok = head.starts_with(b"HTTP/1.1 200");
    let chunked = head
        .windows(20)
        .any(|window| window.eq_ignore_ascii_case(b"transfer-encoding:"));
    let body = if chunked {
        decode_chunked(payload)?
    } else {
        payload.to_vec()
    };
    if !status_ok {
        let first_line = head
            .split(|byte| *byte == b'\n')
            .next()
            .map(|line| String::from_utf8_lossy(line).trim().to_string())
            .unwrap_or_default();
        return Err(format!("管理面返回非 200: {first_line}"));
    }
    Ok(String::from_utf8_lossy(&body).to_string())
}

/// 拆出 HTTP 头与响应体(按 CRLFCRLF 分隔)。
pub fn split_head_and_body(raw: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let separator = b"\r\n\r\n";
    raw.windows(separator.len())
        .position(|window| window == separator)
        .map(|index| {
            let head = &raw[..index];
            let body = &raw[index + separator.len()..];
            (head, body)
        })
        .ok_or_else(|| "HTTP 响应格式异常(缺少 CRLFCRLF 分隔)".to_string())
}

/// 解 chunked 传输编码(逐块读长度行 → 取内容 → 跳过 CRLF)。
pub fn decode_chunked(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        let line_end = rest
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| "chunked 响应格式异常(缺少分块长度行)".to_string())?;
        let size_text = String::from_utf8_lossy(&rest[..line_end]);
        // 分块长度行可能带扩展:`1a;foo=bar` → 只取分号前部分。
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|error| format!("chunked 分块长度非法({size_text:?}): {error}"))?;
        if size == 0 {
            break;
        }
        rest = &rest[line_end + 2..];
        if rest.len() < size {
            return Err("chunked 响应体被截断".to_string());
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size..];
        if rest.len() < 2 || &rest[..2] != b"\r\n" {
            return Err("chunked 分块结束符缺失".to_string());
        }
        rest = &rest[2..];
    }
    Ok(out)
}
