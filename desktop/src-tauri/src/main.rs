// WRouter Tauri 桌面壳入口(方案 20261102_01 · P3)。
//
// release 构建不弹控制台窗口(与 electron-builder 的 GUI 形态一致);
// 启动失败信息走 stderr + 弹窗(见 lib.rs::report_fatal)。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    wrouter_desktop_lib::run();
}
