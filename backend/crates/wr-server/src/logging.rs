//! tracing 管道(方案 #14 R1.6)。
//!
//! 行格式与 Python `cli.py::LOG_FORMAT` 完全一致:
//! `%(asctime)s.%(msecs)03d [backend] [%(levelname)s] [%(name)s]:  %(message)s`
//! 即 `2026-08-29 09:45:01.123 [backend] [INFO] [wrouter.cli]:  message`。
//! 差异说明:`[name]` 槽位填 Rust 的 tracing target(模块路径),
//! 无法逐字复刻 Python logger 名;行结构、时间戳精度、级别词
//! (WARNING 而非 WARN)与 Python 保持一致,运维脚本可按行结构 grep。
//!
//! 脱敏钩子([`redact`])在事件写出前生效(C 系列约束:不向日志
//! 泄漏 API Key),规则随 R2+ 转发面接入持续扩充。

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::Event;
use tracing::Level;
use tracing::Subscriber;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

/// 统一行格式(对齐 Python LOG_FORMAT,见模块文档)。
struct UnifiedFormat;

impl<S, N> FormatEvent<S, N> for UnifiedFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut writer = writer;
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        write!(
            writer,
            "{} [backend] [{}] [{}]:  ",
            timestamp,
            python_level(event.metadata().level()),
            event.metadata().target()
        )?;
        // 先把 message + 字段格式化进缓冲,脱敏后写出。
        let mut fields = String::new();
        ctx.format_fields(Writer::new(&mut fields), event)?;
        // 行尾换行必须由本 formatter 负责(默认 formatter 自带);
        // 缺失会让 stdout LineWriter 永不刷新。
        writeln!(writer, "{}", redact(&fields))
    }
}

/// tracing 级别 → Python logging 级别词。
fn python_level(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARNING",
        Level::INFO => "INFO",
        Level::DEBUG | Level::TRACE => "DEBUG",
    }
}

/// 运行日志文件基名(桌面壳写入 `<log_dir>/backend.log`)。
const LOG_FILE_BASE: &str = "backend";
/// 单文件轮转阈值(旧 Electron 壳约定:5MB)。
const DEFAULT_LOG_MAX_SIZE_MB: u64 = 5;
/// 归档保留数(旧 Electron 壳约定:9 个)。
const DEFAULT_LOG_MAX_FILES: usize = 9;

fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

fn env_positive(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

/// 滚动文件:按大小轮转到 `<base>-YYYYMMDD-HHMMSS[-n].log`,只保留最近
/// `max_files` 个归档。所有 I/O 失败都静默吞掉 —— 日志绝不能影响主流程
/// (与旧 Electron `log-writer.ts` 的立场一致)。
struct RollingFile {
    dir: PathBuf,
    base: String,
    max_bytes: u64,
    max_files: usize,
    file: Option<File>,
    size: u64,
}

impl RollingFile {
    fn new(dir: PathBuf, base: &str, max_bytes: u64, max_files: usize) -> Self {
        Self {
            dir,
            base: base.to_owned(),
            max_bytes: max_bytes.max(1),
            max_files,
            file: None,
            size: 0,
        }
    }

    fn active_path(&self) -> PathBuf {
        self.dir.join(format!("{}.log", self.base))
    }

    fn ensure_open(&mut self) -> io::Result<()> {
        if self.file.is_some() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.active_path())?;
        self.size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        self.file = Some(file);
        Ok(())
    }

    fn write_line(&mut self, bytes: &[u8]) {
        let _ = self.try_write(bytes);
    }

    fn try_write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.ensure_open()?;
        if self.size > 0 && self.size.saturating_add(bytes.len() as u64) > self.max_bytes {
            self.rotate()?;
            // 轮转成功 = 新文件;rename 失败 = 旧文件仍原地,两种都要重新打开。
            self.ensure_open()?;
        }
        if let Some(file) = self.file.as_mut() {
            file.write_all(bytes)?;
        }
        self.size = self.size.saturating_add(bytes.len() as u64);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        // 先关闭句柄再改名,避免 Windows 上 rename 被占用失败。
        self.file = None;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let mut archive = self.dir.join(format!("{}-{}.log", self.base, stamp));
        let mut suffix = 1u32;
        while archive.exists() {
            archive = self
                .dir
                .join(format!("{}-{}-{suffix:03}.log", self.base, stamp));
            suffix = suffix.saturating_add(1);
        }
        if std::fs::rename(self.active_path(), &archive).is_ok() {
            self.size = 0;
        }
        self.cleanup_archives();
        Ok(())
    }

    fn cleanup_archives(&self) {
        if self.max_files == 0 {
            return;
        }
        let prefix = format!("{}-", self.base);
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut archives: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".log"))
            })
            .collect();
        archives.sort();
        if archives.len() > self.max_files {
            for path in &archives[..archives.len() - self.max_files] {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// tracing 的 `MakeWriter` 适配:每次 `write` 加锁写文件。
#[derive(Clone)]
struct RollingWriter {
    inner: Arc<Mutex<RollingFile>>,
}

impl RollingWriter {
    fn new(dir: &Path, base: &str, max_bytes: u64, max_files: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RollingFile::new(
                dir.to_path_buf(),
                base,
                max_bytes,
                max_files,
            ))),
        }
    }

    /// 进程启动分隔行(旧 Electron 壳同款,便于运维按 session 切分日志)。
    fn session_start(&self) {
        let line = format!(
            "==== session start {} pid={} version={} ====\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            std::process::id(),
            crate::VERSION
        );
        let mut file = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        file.write_line(line.as_bytes());
    }
}

struct RollingGuard<'a> {
    file: std::sync::MutexGuard<'a, RollingFile>,
}

impl Write for RollingGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write_line(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RollingWriter {
    type Writer = RollingGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        RollingGuard {
            file: self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        }
    }
}

/// 初始化全局 tracing(RUST_LOG 级别过滤,缺省 info,对齐 Python
/// `logging.basicConfig(level=logging.INFO)`)。仅 stdout(CLI / systemd)。
pub fn init() {
    let layer = tracing_subscriber::fmt::layer()
        .event_format(UnifiedFormat)
        .with_writer(std::io::stdout);
    let _ = tracing_subscriber::registry()
        .with(env_filter())
        .with(layer)
        .try_init();
}

/// 初始化全局 tracing 并同时写入滚动文件(桌面壳用)。
///
/// 文件按 `WROUTER_LOG_MAX_SIZE_MB`(默认 5)轮转,保留
/// `WROUTER_LOG_MAX_FILES`(默认 9)个归档;目录不存在会自动创建。
/// 返回 `false` = 全局 subscriber 已初始化过(不重复安装)。
pub fn init_with_file(log_dir: &Path) -> bool {
    let max_bytes = env_positive("WROUTER_LOG_MAX_SIZE_MB", DEFAULT_LOG_MAX_SIZE_MB)
        .saturating_mul(1024 * 1024);
    let max_files = env_positive("WROUTER_LOG_MAX_FILES", DEFAULT_LOG_MAX_FILES as u64) as usize;
    let writer = RollingWriter::new(log_dir, LOG_FILE_BASE, max_bytes, max_files);
    let session = writer.clone();
    let stdout_layer = tracing_subscriber::fmt::layer()
        .event_format(UnifiedFormat)
        .with_writer(std::io::stdout);
    let file_layer = tracing_subscriber::fmt::layer()
        .event_format(UnifiedFormat)
        .with_ansi(false)
        .with_writer(writer);
    let installed = tracing_subscriber::registry()
        .with(env_filter())
        .with(stdout_layer)
        .with(file_layer)
        .try_init()
        .is_ok();
    if installed {
        session.session_start();
    }
    installed
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_secret_char(b: u8) -> bool {
    is_word_byte(b) || b == b'-'
}

/// 敏感字段脱敏(C 系列约束的全局日志钩子)。
///
/// 规则:
/// 1. `sk-` 开头的密钥形态(≥6 个密钥字符)→ `sk-***`;
/// 2. `Bearer <token>` → `Bearer ***`;
/// 3. URL 内嵌凭据 `scheme://user:pass@host` → `scheme://host`
///    (对齐 Python cli.py 诊断日志的 `split("@")` 行为);
/// 4. `api_key=`/`apiKey=`/`api-key=`(含 `:` 分隔)赋值 → `api_key=***`。
pub fn redact(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        // 规则 3:URL 凭据。
        if input[i..].starts_with("://") {
            out.push_str("://");
            i += 3;
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' {
                i += 1;
            }
            let authority = &input[start..i];
            match authority.rfind('@') {
                Some(at) => out.push_str(&authority[at + 1..]),
                None => out.push_str(authority),
            }
            continue;
        }
        // 规则 2:Bearer 令牌。
        if input[i..].starts_with("Bearer ") || input[i..].starts_with("bearer ") {
            let mut j = i + 7;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j > i + 7 {
                out.push_str("Bearer ***");
                i = j;
                continue;
            }
        }
        // 规则 4:api_key 类赋值。
        if let Some(rest_len) = api_key_prefix_len(&input[i..]) {
            let mut j = i + rest_len;
            // 分隔符后的空格属于赋值语法的一部分(apiKey: "x")。
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                // 引号包裹的值:吃到闭合引号。
                j += 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += 1;
                }
                if j < bytes.len() {
                    j += 1;
                }
                out.push_str(&input[i..i + rest_len]);
                out.push_str("***");
                i = j;
                continue;
            }
            let value_start = j;
            while j < bytes.len()
                && !bytes[j].is_ascii_whitespace()
                && bytes[j] != b'"'
                && bytes[j] != b'&'
            {
                j += 1;
            }
            if j > value_start {
                out.push_str(&input[i..i + rest_len]);
                out.push_str("***");
                i = j;
                continue;
            }
        }
        // 规则 1:sk- 密钥。
        if input[i..].starts_with("sk-") && (i == 0 || !is_word_byte(bytes[i - 1])) {
            let mut j = i + 3;
            while j < bytes.len() && is_secret_char(bytes[j]) {
                j += 1;
            }
            if j - (i + 3) >= 6 {
                out.push_str("sk-***");
                i = j;
                continue;
            }
        }
        // 未命中规则:按字节原样拷贝(保持 UTF-8 多字节序列完整)。
        let mut end = i + 1;
        while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
            end += 1;
        }
        out.push_str(&input[i..end]);
        i = end;
    }
    out
}

/// 识别 `api_key`/`apiKey`/`api-key` 后跟 `=` 或 `:` 的前缀长度。
fn api_key_prefix_len(rest: &str) -> Option<usize> {
    for name in ["api_key", "apiKey", "api-key"] {
        for sep in ['=', ':'] {
            let prefix = format!("{name}{sep}");
            if rest.starts_with(&prefix) {
                return Some(prefix.len());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{RollingWriter, python_level, redact};
    use std::path::PathBuf;
    use tracing::Level;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wr-log-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn rolling_file_rotates_and_retains_archives() {
        let dir = temp_dir("rotate");
        let writer = RollingWriter::new(&dir, "backend", 32, 2);
        for _ in 0..12 {
            writer.inner.lock().unwrap().write_line(b"0123456789\n");
        }
        let active = std::fs::metadata(dir.join("backend.log")).unwrap().len();
        assert!(
            active <= 32,
            "active file must respect the size cap: {active}"
        );
        let archives = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("backend-") && name.ends_with(".log")
            })
            .count();
        assert!(archives > 0, "expected at least one rotated archive");
        assert!(archives <= 2, "archive retention must cap at 2: {archives}");

        writer.session_start();
        let active_text = std::fs::read_to_string(dir.join("backend.log")).unwrap_or_default();
        assert!(active_text.contains("==== session start"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 端到端验证 file layer 已接上:init 后 tracing 事件必须落进 backend.log。
    #[test]
    fn init_with_file_writes_tracing_events() {
        let dir = temp_dir("init");
        assert!(super::init_with_file(&dir), "file layer should install");
        tracing::info!(target: "wrouter.test", marker = "hello-file", "log test");
        let text = std::fs::read_to_string(dir.join("backend.log")).unwrap_or_default();
        assert!(
            text.contains("==== session start"),
            "missing session marker: {text}"
        );
        assert!(text.contains("hello-file"), "missing tracing event: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn level_names_match_python_logging() {
        assert_eq!(python_level(&Level::WARN), "WARNING");
        assert_eq!(python_level(&Level::INFO), "INFO");
        assert_eq!(python_level(&Level::ERROR), "ERROR");
        assert_eq!(python_level(&Level::DEBUG), "DEBUG");
    }

    #[test]
    fn redacts_sk_keys() {
        assert_eq!(
            redact("using key sk-abcd1234xyz in request"),
            "using key sk-*** in request"
        );
        // 短于 6 位的不算密钥形态,避免误伤普通词。
        assert_eq!(redact("sk-ab"), "sk-ab");
        // 词中出现的 sk- 不处理(task- 前缀词等)。
        assert_eq!(redact("mask-123456"), "mask-123456");
    }

    #[test]
    fn redacts_bearer_tokens() {
        assert_eq!(
            redact("Authorization: Bearer abc.def-ghi"),
            "Authorization: Bearer ***"
        );
    }

    #[test]
    fn redacts_url_credentials_like_python_cli() {
        assert_eq!(
            redact("proxy http://user:pass@proxy.corp:8080 via env"),
            "proxy http://proxy.corp:8080 via env"
        );
        assert_eq!(
            redact("https://user@host.com/path"),
            "https://host.com/path"
        );
        // 无凭据的 URL 不变。
        assert_eq!(
            redact("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1"
        );
    }

    #[test]
    fn redacts_api_key_assignments() {
        assert_eq!(redact("api_key=sk-real-secret here"), "api_key=*** here");
        assert_eq!(redact("apiKey: \"quoted\""), "apiKey:***");
        assert_eq!(redact("api-key=abc123&next=1"), "api-key=***&next=1");
    }

    #[test]
    fn preserves_multibyte_text() {
        assert_eq!(
            redact("日志:sk-abcd12345678 中文 ok"),
            "日志:sk-*** 中文 ok"
        );
    }
}
