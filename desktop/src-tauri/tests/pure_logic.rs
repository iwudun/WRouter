//! 桌面壳**纯逻辑**单元测试(20261102_01 P3-4)。
//!
//! 为什么要单独一个测试目标:
//!
//! `desktop/src-tauri` 依赖 tauri → tauri-runtime-wry → webview2-com,
//! 而 **WebView2 走动态装载**(`WebView2Loader.dll`)。在 Windows GNU
//! 工具链下,`cargo test` 生成的 harness 二进制落在 `target/<profile>/deps/`,
//! 找不到装载器 → 进程起不来(0xC0000135 / 0xC0000139)。可执行文件本身
//! 不受影响(它与 DLL 同目录)—— 见方案文档 §9.4 踩坑清单。
//!
//! 所以把**不依赖 tauri 的纯逻辑**(`http.rs` / `bridge_core.rs`)用
//! `#[path]` 引入本测试目标:该目标不引用 lib crate → 链接器不会拉进
//! WebView2 装载器 → 单元测试在本机(Windows GNU)也能真跑。
//!
//! 用法:`cd desktop/src-tauri && cargo test --test pure_logic`

#[path = "../src/bridge_core.rs"]
mod bridge_core;

// Q-25:测试目标只调用 http 的分帧/解块函数,生产用的 `post` 在此被判
// dead code 是测试结构造成的假告警 —— 保留生产函数,只在本测试模块静音。
#[allow(dead_code)]
#[path = "../src/http.rs"]
mod http;

#[path = "../src/shell_core.rs"]
mod shell_core;

use bridge_core::{DESKTOP_METHODS, init_script, normalize_external_http_url};
use shell_core::{
    parse_hex_color, resolve_window_theme, WindowTheme, BACKGROUND_DARK, BACKGROUND_LIGHT,
};

#[test]
fn rejects_non_http_urls() {
    assert!(normalize_external_http_url("file:///C:/Windows").is_err());
    assert!(normalize_external_http_url("javascript:alert(1)").is_err());
    assert!(normalize_external_http_url("not a url").is_err());
    assert!(normalize_external_http_url("http://").is_err());
}

#[test]
fn accepts_http_urls() {
    assert!(normalize_external_http_url("https://example.com").is_ok());
    assert!(normalize_external_http_url("http://127.0.0.1:3462/").is_ok());
}

#[test]
fn init_script_declares_desktop_runtime() {
    let script = init_script(true);
    assert!(script.contains("runtime: \"desktop\""));
    assert!(script.contains("var isDev = true;"));
    assert!(!script.contains("__IS_DEV__"));
}

#[test]
fn init_script_declares_every_desktop_method() {
    let script = init_script(false);
    for method in DESKTOP_METHODS {
        assert!(
            script.contains(&format!("{method}: function")),
            "init script 缺少桌面方法 {method}"
        );
    }
}

#[test]
fn desktop_methods_match_the_contract_shape() {
    // 与 contracts/wrouter-capabilities.json 的 desktopMethods 同集合
    // (8 项)。真正的三方一致性由 scripts/validate-capabilities.mjs 校验,
    // 这里只锁住「数量不变」这个约定。
    assert_eq!(DESKTOP_METHODS.len(), 8);
    for method in [
        "exportData",
        "getUpdateStatus",
        "openExternal",
        "setAppLanguage",
        "toggleDevTools",
        "updateCheck",
        "updateDownload",
        "updateInstall",
    ] {
        assert!(
            DESKTOP_METHODS.contains(&method),
            "桌面方法清单缺少 {method}"
        );
    }
}

#[test]
fn splits_head_and_body() {
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";
    let (head, body) = http::split_head_and_body(raw).unwrap();
    assert!(head.starts_with(b"HTTP/1.1 200"));
    assert_eq!(body, b"hi");
}

#[test]
fn decodes_chunked_body() {
    let raw = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
    assert_eq!(http::decode_chunked(raw).unwrap(), b"hello world");
}

#[test]
fn decodes_chunked_body_with_extension() {
    let raw = b"5;foo=bar\r\nhello\r\n0\r\n\r\n";
    assert_eq!(http::decode_chunked(raw).unwrap(), b"hello");
}

#[test]
fn rejects_truncated_chunk() {
    let raw = b"5\r\nhel";
    assert!(http::decode_chunked(raw).is_err());
}

// ---------------------------------------------------------------------------
// 窗口层纯逻辑(20261102_01 P4-4 / P4-5)
// ---------------------------------------------------------------------------

#[test]
fn parses_six_digit_hex_color() {
    assert_eq!(parse_hex_color("#f4f4f6"), Some((0xf4, 0xf4, 0xf6)));
    assert_eq!(parse_hex_color("1a1a1e"), Some((0x1a, 0x1a, 0x1e)));
    assert_eq!(parse_hex_color("  #1A1A1E  "), Some((0x1a, 0x1a, 0x1e)));
}

#[test]
fn expands_three_digit_hex_color() {
    // CSS 规则:#abc → #aabbcc
    assert_eq!(parse_hex_color("#abc"), Some((0xaa, 0xbb, 0xcc)));
}

#[test]
fn rejects_invalid_hex_color() {
    assert_eq!(parse_hex_color(""), None);
    assert_eq!(parse_hex_color("#"), None);
    assert_eq!(parse_hex_color("#12345"), None);
    assert_eq!(parse_hex_color("#gggggg"), None);
    assert_eq!(parse_hex_color("rgb(1,2,3)"), None);
}

/// 显式 light/dark → 强制窗口主题;system → **不强制**
/// (强制会把 webview 的 prefers-color-scheme 钉死,系统主题运行期切换传不进来)。
#[test]
fn window_theme_preference_parses() {
    assert_eq!(WindowTheme::parse("light"), WindowTheme::Light);
    assert_eq!(WindowTheme::parse("DARK"), WindowTheme::Dark);
    assert_eq!(WindowTheme::parse(" system "), WindowTheme::System);
    // 未知值 → 跟随系统(与前端 normalizeThemePreference 的兜底一致)
    assert_eq!(WindowTheme::parse("whatever"), WindowTheme::System);
    assert!(WindowTheme::Light.is_forced());
    assert!(WindowTheme::Dark.is_forced());
    assert!(!WindowTheme::System.is_forced());
}

/// 底色优先级:前端给的 #rrggbb > 主题默认色。
#[test]
fn resolve_window_theme_prefers_explicit_background() {
    let (theme, color) = resolve_window_theme("dark", Some("#123456"), false);
    assert_eq!(theme, WindowTheme::Dark);
    assert_eq!(color, (0x12, 0x34, 0x56));

    // 底色非法 → 退回主题默认色
    let (theme, color) = resolve_window_theme("dark", Some("nope"), false);
    assert_eq!(theme, WindowTheme::Dark);
    assert_eq!(color, BACKGROUND_DARK);
}

/// system 偏好下,默认底色跟系统明暗走(不钉死某一种)。
#[test]
fn resolve_window_theme_system_follows_system_color() {
    let (theme, color) = resolve_window_theme("system", None, true);
    assert_eq!(theme, WindowTheme::System);
    assert_eq!(color, BACKGROUND_DARK);

    let (_, color) = resolve_window_theme("system", None, false);
    assert_eq!(color, BACKGROUND_LIGHT);
}

/// 底色常量必须与前端 CSS 变量 `--background` 同值
/// (浅色 #f4f4f6 / 深色 #1a1a1e),否则最大化时会闪白/闪黑。
#[test]
fn background_constants_match_frontend_palette() {
    assert_eq!(BACKGROUND_LIGHT, (0xf4, 0xf4, 0xf6));
    assert_eq!(BACKGROUND_DARK, (0x1a, 0x1a, 0x1e));
}
