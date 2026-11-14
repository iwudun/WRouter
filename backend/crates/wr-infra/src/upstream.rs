//! 上游 HTTP 客户端(方案 #14 R2.3/D2:reqwest + rustls)。
//!
//! 超时语义对齐 Python httpx(单一 `provider.timeout_seconds`,默认
//! 120s,作用于**各阶段独立**而非总时长),另有一个 Rust 修正
//! (2026-10-08,非流式失败率高的根因):
//! - 连接:`connect_timeout(timeout_seconds)`;
//! - 读:每个读操作(非流式整包按 chunk 循环、流式逐 chunk)套
//!   `tokio::time::timeout(timeout_seconds)` —— 慢速但活跃的流不会
//!   被总时长掐断(镜像 httpx read timeout 的逐读语义);
//! - **非流式「等响应头」单独用 `non_stream_timeout_seconds`(默认
//!   600s)**:非流式上游要把整个响应生成完才回响应头,等待头 ≈ 完整
//!   生成时长,复用 120s 会把长生成整段掐断(流式头即刻返回,无此
//!   问题);正文到达后的逐读仍用 timeout_seconds(正文早已生成完,
//!   传输很快);
//! - 不设 reqwest 总超时(`Client::builder().timeout()` 会覆盖整个
//!   请求生命周期,长流会被误杀,与 Python 行为相反)。
//!
//! 其他对齐:`follow_redirects`(上限 20,httpx 默认)、响应透明解压
//! (gzip/deflate/brotli,httpx 默认行为,relay 需要解压后字节)、
//! 共享连接池(按 provider 缓存 client,镜像 Python `_client`)。

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Map, Value};

use crate::proxy::ProxyDecision;
use wr_domain::provider::Provider;

/// 非流式「等响应头」缺省超时(秒)。常见默认同量级;长非流式
/// 生成(大上下文/慢模型)普遍分钟级,120s 的逐读默认在此不够用。
pub const DEFAULT_NON_STREAM_TIMEOUT_SECS: f64 = 600.0;

/// 非流式「等响应头」超时:provider 显式覆盖优先,否则落缺省。
fn non_stream_header_timeout(provider: &Provider) -> Duration {
    Duration::from_secs_f64(
        provider
            .non_stream_timeout_seconds
            .unwrap_or(DEFAULT_NON_STREAM_TIMEOUT_SECS)
            .max(0.0),
    )
}

/// 上游请求失败类型(镜像 httpx.HTTPError / TimeoutError 的两条路径)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    /// 连接失败 / TLS / DNS / 上游主动断开。
    Transport(String),
    /// 单次读操作超时(镜像 httpx read timeout)。
    ReadTimeout,
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "{message}"),
            Self::ReadTimeout => write!(f, "upstream read timed out"),
        }
    }
}

impl std::error::Error for UpstreamError {}

/// 上游响应的头部集合(header 名一律小写,与 httpx 一致)。
pub type HeaderMap = Vec<(String, String)>;

/// 非流式响应(整包已读)。
#[derive(Debug)]
pub struct UpstreamResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl UpstreamResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn content_type(&self) -> &str {
        self.header("content-type").unwrap_or("")
    }
}

/// 流式响应:status/headers 先到,body 以 chunk 迭代(逐 chunk 读超时)。
pub struct UpstreamStream {
    pub status: u16,
    pub headers: HeaderMap,
    body: futures_util::stream::BoxStream<'static, Result<Vec<u8>, UpstreamError>>,
}

impl UpstreamStream {
    pub async fn next_chunk(&mut self) -> Option<Result<Vec<u8>, UpstreamError>> {
        self.body.next().await
    }
}

/// 共享上游客户端池(按 `provider.id + 代理模式` 缓存,镜像 Python
/// `_client` 的共享缓存语义;客户端不得按请求重建)。
#[derive(Default)]
pub struct UpstreamClient {
    clients: tokio::sync::Mutex<HashMap<String, reqwest::Client>>,
}

impl UpstreamClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取(或建)provider 对应的 client。
    pub async fn client_for(&self, provider: &Provider, proxy: &ProxyDecision) -> reqwest::Client {
        let cache_key = format!("{}|{proxy:?}", provider.id);
        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(&cache_key) {
            return client.clone();
        }
        let timeout = Duration::from_secs_f64(provider.timeout_seconds.max(0.0));
        let mut builder = reqwest::Client::builder()
            .connect_timeout(timeout)
            .redirect(reqwest::redirect::Policy::limited(20));
        builder = match proxy {
            ProxyDecision::Direct => builder.no_proxy(),
            ProxyDecision::Url(url) => match reqwest::Proxy::all(url) {
                Ok(proxy) => builder.proxy(proxy),
                Err(_) => builder.no_proxy(),
            },
        };
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        clients.insert(cache_key, client.clone());
        client
    }

    fn build_request(
        client: &reqwest::Client,
        url: &str,
        headers: &Map<String, Value>,
        body: Vec<u8>,
    ) -> Result<reqwest::RequestBuilder, UpstreamError> {
        let mut request = client.post(url);
        for (key, value) in headers {
            let Some(text) = value.as_str() else {
                continue;
            };
            request = request.header(key.as_str(), text);
        }
        Ok(request.body(body))
    }

    /// 带逐读超时的整包读取(镜像 httpx 逐读语义)。
    async fn read_all(
        mut response: reqwest::Response,
        read_timeout: Duration,
    ) -> Result<Vec<u8>, UpstreamError> {
        let mut buffer: Vec<u8> = Vec::new();
        loop {
            match tokio::time::timeout(read_timeout, response.chunk()).await {
                Ok(Ok(Some(chunk))) => buffer.extend_from_slice(&chunk),
                Ok(Ok(None)) => return Ok(buffer),
                Ok(Err(e)) => return Err(reqwest_to_upstream(e)),
                Err(_) => return Err(UpstreamError::ReadTimeout),
            }
        }
    }

    /// 非流式 POST(JSON 体;content-type 由 upstream_headers 提供)。
    pub async fn post_json(
        &self,
        provider: &Provider,
        proxy: &ProxyDecision,
        url: &str,
        headers: &Map<String, Value>,
        body: &Value,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let payload = serde_json::to_vec(body)
            .map_err(|e| UpstreamError::Transport(format!("serialize request body: {e}")))?;
        self.post_json_bytes(provider, proxy, url, headers, &payload)
            .await
    }

    /// 非流式 POST(原始字节体;P3-1 原生请求面 ByteExact 透传/定点修改)。
    /// 调用方已持有最终出站字节,本层不再重序列化。
    pub async fn post_json_bytes(
        &self,
        provider: &Provider,
        proxy: &ProxyDecision,
        url: &str,
        headers: &Map<String, Value>,
        payload: &[u8],
    ) -> Result<UpstreamResponse, UpstreamError> {
        let client = self.client_for(provider, proxy).await;
        let request = Self::build_request(&client, url, headers, payload.to_vec())?;
        let read_timeout = Duration::from_secs_f64(provider.timeout_seconds.max(0.0));
        // 等响应头用非流式专用超时:头到达 = 整个响应生成完(见模块文档);
        // 之后的整包读取仍按逐读 timeout_seconds 计。
        let response = tokio::time::timeout(non_stream_header_timeout(provider), request.send())
            .await
            .map_err(|_| UpstreamError::ReadTimeout)?
            .map_err(reqwest_to_upstream)?;
        let status = response.status().as_u16();
        let header_map = collect_headers(response.headers());
        let body = Self::read_all(response, read_timeout).await?;
        Ok(UpstreamResponse {
            status,
            headers: header_map,
            body,
        })
    }

    /// 流式 POST(镜像 `client.stream(...)` + `aiter_bytes()`):
    /// 逐 chunk 字节流,每个读操作带独立超时。
    pub async fn post_stream(
        &self,
        provider: &Provider,
        proxy: &ProxyDecision,
        url: &str,
        headers: &Map<String, Value>,
        body: &Value,
    ) -> Result<UpstreamStream, UpstreamError> {
        let payload = serde_json::to_vec(body)
            .map_err(|e| UpstreamError::Transport(format!("serialize request body: {e}")))?;
        self.post_stream_bytes(provider, proxy, url, headers, &payload)
            .await
    }

    /// 流式 POST(原始字节体;P3-1 原生请求面 ByteExact 透传/定点修改)。
    pub async fn post_stream_bytes(
        &self,
        provider: &Provider,
        proxy: &ProxyDecision,
        url: &str,
        headers: &Map<String, Value>,
        payload: &[u8],
    ) -> Result<UpstreamStream, UpstreamError> {
        let client = self.client_for(provider, proxy).await;
        let request = Self::build_request(&client, url, headers, payload.to_vec())?;
        let read_timeout = Duration::from_secs_f64(provider.timeout_seconds.max(0.0));
        // 同上:首字节(响应头)等待受 read timeout 约束。
        let response = tokio::time::timeout(read_timeout, request.send())
            .await
            .map_err(|_| UpstreamError::ReadTimeout)?
            .map_err(reqwest_to_upstream)?;
        let status = response.status().as_u16();
        let header_map = collect_headers(response.headers());
        let chunks = response.bytes_stream().map(|item| {
            item.map(|chunk| chunk.to_vec())
                .map_err(reqwest_to_upstream)
        });
        // 逐 chunk 超时:慢速但活跃的流不会被总时长掐断。
        let body_stream = futures_util::stream::unfold(
            (chunks, read_timeout),
            |(mut chunks, read_timeout)| async move {
                match tokio::time::timeout(read_timeout, chunks.next()).await {
                    Ok(Some(Ok(chunk))) => Some((Ok(chunk), (chunks, read_timeout))),
                    Ok(Some(Err(e))) => Some((Err(e), (chunks, read_timeout))),
                    Ok(None) => None,
                    Err(_) => Some((Err(UpstreamError::ReadTimeout), (chunks, read_timeout))),
                }
            },
        );
        Ok(UpstreamStream {
            status,
            headers: header_map,
            body: Box::pin(body_stream),
        })
    }

    #[cfg(test)]
    pub async fn cached_clients(&self) -> usize {
        self.clients.lock().await.len()
    }
}

/// reqwest 错误 → 传输错误(错误串经 [`reqwest_error_message`] 脱敏)。
fn reqwest_to_upstream(error: reqwest::Error) -> UpstreamError {
    UpstreamError::Transport(reqwest_error_message(&error))
}

/// reqwest 错误的脱敏 Display:reqwest 会把完整请求 URL 内嵌进错误
/// 消息,而 Gemini 线的 query 鉴权(`x-wrouter-gemini-key-mode: query`,
/// protocol::gemini_query_key)把 api_key 放在 `?key=…` —— 错误串会
/// 原样进调用日志 error 字段/尝试明细/探针结果,这里先把 URL 的
/// query 与 fragment 剥掉(2026-10-17 脱敏审查)。
pub fn reqwest_error_message(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    if let Some(url) = error.url() {
        message = strip_url_query_in_message(message, url.as_str());
    }
    message
}

/// 把消息里内嵌的完整 URL 替换为剥掉 query/fragment 的形态(纯函数,
/// 便于测试;reqwest Error 无法手工构造)。
fn strip_url_query_in_message(message: String, url: &str) -> String {
    if !message.contains(url) {
        return message;
    }
    let mut stripped = url.to_owned();
    if let Some(cut) = stripped.find(['?', '#']) {
        stripped.truncate(cut);
    }
    message.replace(url, &stripped)
}

fn collect_headers(headers: &reqwest::header::HeaderMap) -> HeaderMap {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_provider() -> Provider {
        let mut raw = Map::new();
        raw.insert("id".into(), json!("prov-p"));
        raw.insert("api_base_url".into(), json!("http://127.0.0.1:1/p"));
        Provider::from_raw(&raw).unwrap()
    }

    #[test]
    fn non_stream_header_timeout_defaults_and_overrides() {
        let mut provider = make_provider();
        assert_eq!(
            non_stream_header_timeout(&provider),
            Duration::from_secs_f64(DEFAULT_NON_STREAM_TIMEOUT_SECS),
            "未显式设置落 600s 默认(非流式长生成不再被 120s 掐断)"
        );
        provider.non_stream_timeout_seconds = Some(900.0);
        assert_eq!(
            non_stream_header_timeout(&provider),
            Duration::from_secs(900)
        );
    }

    #[test]
    fn transport_error_strips_url_query_credentials() {
        // Gemini query 鉴权线:key 直接在 URL query 里,传输错误内嵌完整
        // URL 时不得把 api_key 带进日志 error 字段(2026-10-17 脱敏审查)。
        let url = "https://api.example.com/v1beta/models/m:generateContent?key=AIzaSecret&alt=sse";
        let message = format!("error sending request for url ({url})");
        let scrubbed = strip_url_query_in_message(message, url);
        assert_eq!(
            scrubbed,
            "error sending request for url (https://api.example.com/v1beta/models/m:generateContent)"
        );
        assert!(!scrubbed.contains("AIzaSecret"), "{scrubbed}");
        // 消息不含该 URL 时原样返回。
        assert_eq!(
            strip_url_query_in_message("connect refused".to_owned(), url),
            "connect refused"
        );
        // 无 query/fragment 的 URL 保持原样。
        let plain = "https://api.example.com/v1/chat/completions";
        let message = format!("error sending request for url ({plain})");
        assert_eq!(strip_url_query_in_message(message.clone(), plain), message);
    }

    #[tokio::test]
    async fn client_is_cached_per_provider() {
        let client = UpstreamClient::new();
        let provider = make_provider();
        let _ = client.client_for(&provider, &ProxyDecision::Direct).await;
        let _ = client.client_for(&provider, &ProxyDecision::Direct).await;
        assert_eq!(client.cached_clients().await, 1, "同 provider 应命中缓存");
        let mut other = make_provider();
        other.id = "prov-other".to_owned();
        let _ = client.client_for(&other, &ProxyDecision::Direct).await;
        assert_eq!(client.cached_clients().await, 2);
    }
}
