use std::path::{Path, PathBuf};

fn main() {
    // 生成 tauri.conf.json 的派生代码 + 权限清单(capabilities/permissions)。
    tauri_build::build();
    copy_webview2_loader();
}

/// 把 WebView2 装载 DLL 复制到 `target/<profile>/` 与 `target/<profile>/deps/`。
///
/// 背景(20261102_01 P3):WebView2 走**动态**装载 —— `webview2-com-sys` 的
/// build.rs 会把 `WebView2Loader.dll` 放到自己的 OUT_DIR,链接时产生
/// `WebView2Loader.dll` 的 import 表项。于是:
///
/// - 可执行文件在 `target/<profile>/` → 运行时能找到同目录的 DLL ✅;
/// - `cargo test` 生成的 harness 二进制在 `target/<profile>/deps/` →
///   找不到 DLL → 进程起不来(0xC0000135 DLL_NOT_FOUND)❌。
///
/// 依赖的 build.rs 先于本 crate 执行,所以这里能确定性地拿到 DLL 源文件。
#[cfg(target_os = "windows")]
fn copy_webview2_loader() {
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    // OUT_DIR = <target>/<profile>/build/<crate>-<hash>/out
    let Some(profile_root) = Path::new(&out_dir)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
    else {
        return;
    };
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        _ => "x64",
    };
    let Some(source) = find_webview2_dll(&profile_root.join("build"), arch) else {
        return;
    };
    for directory in [profile_root.clone(), profile_root.join("deps")] {
        let _ = std::fs::create_dir_all(&directory);
        let destination = directory.join("WebView2Loader.dll");
        let _ = std::fs::copy(&source, &destination);
    }
}

#[cfg(target_os = "windows")]
fn find_webview2_dll(build_dir: &Path, arch: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(build_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("webview2-com-sys-") {
            continue;
        }
        let candidate = entry.path().join("out").join(arch).join("WebView2Loader.dll");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn copy_webview2_loader() {}
