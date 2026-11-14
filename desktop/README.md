# WRouter Tauri 桌面壳

**Windows 侧唯一的桌面壳**（Tauri v2；原 Electron 壳已退场，目录已删除）。

## 它是什么

- **独立 Cargo 工程**（`desktop/src-tauri/Cargo.toml` 带 `[workspace]`）——自己的 `Cargo.lock`，
  Tauri 的 500+ 个依赖包**不污染** `backend/` 的 workspace 与 `Cargo.lock`，
  也不影响后端的「wr-domain 纯度守护」与「离线可解」约束。
- **后端同进程**：`wr-server` 是 path 依赖，后端跑在壳内的**独立线程 + 独立 tokio runtime**上
  —— 进程列表里只有桌面壳一个进程，**没有 `wr-backend` 子进程**。
- **窗口加载管理面**：生产形态加载 `http://127.0.0.1:3462/`（后端静态托管同一份 `frontend/dist`，
  不用 `tauri://` 资产协议）；dev 形态加载 vite dev server（HMR）。
- **桥接层**：壳在页面脚本之前注入 `window.wr`（`runtime: "desktop"` + 8 个桌面方法），
  前端 `web-client-bridge.ts` 随后**就地补齐** 28 个业务方法（复用同一套 RPC 逻辑）。
- **Windows 桌面能力**：单实例、开机自启、**自绘标题栏**（去系统标题栏 + 前端 min/max/close）、
  标题栏配色随应用亮暗主题同步。

## 目录

| 路径 | 作用 |
|---|---|
| `src/lib.rs` | 入口：插件注册、命令注册、起后端、建窗口、启动对账、退出时停机 |
| `src/backend.rs` | 后端同进程托管（线程 + runtime + 就绪探测 + 设置装配） |
| `src/bridge.rs` | 8 个 Tauri 命令（`wr_open_external` / `wr_toggle_devtools` / …） |
| `src/bridge_core.rs` | **纯逻辑**：桌面方法清单、注入脚本模板、URL 白名单（不依赖 tauri） |
| `src/shell_window.rs` | 窗口层命令：min/max/close、配色同步、开机自启同步 |
| `src/shell_core.rs` | **纯逻辑**：主题归一 + 十六进制颜色解析（不依赖 tauri） |
| `src/autostart.rs` | 开机自启：OS 登录项幂等同步 |
| `src/config.rs` | 读后端配置（`getConfig` RPC）的桌面壳侧小工具 |
| `src/http.rs` | 极简 HTTP/1.1 客户端（只访问本机回环，不引 reqwest） |
| `tests/pure_logic.rs` | 纯逻辑单测（不链接 WebView2 → 任何工具链都能跑） |
| `capabilities/default.json` | IPC 能力白名单（`remote.urls` 限定回环 3462 与 dev vite 端口） |
| `permissions/desktop.toml` | 放行 8 个桌面命令 + 6 个窗口层命令 |
| `icons/` | Tauri 固定命名的图标集（由 `npm run build:icons` 从 logo 产出） |
| `package.json` | Tauri app dir 的独立 npm 工程（装 `@tauri-apps/cli`） |

## 常用命令

```bash
# 开发（起 vite + Tauri 窗口，后端同进程）
npm run dev                          # 仓库根目录

# 只跑纯逻辑单测（不链接 webview2；桌面壳默认 target 即 msvc）
cd desktop/src-tauri && cargo test --target x86_64-pc-windows-msvc --test pure_logic

# 构建（默认 target 即 MSVC：desktop/src-tauri/.cargo/config.toml）
cd desktop/src-tauri && cargo build

# 打包（Windows NSIS 安装器）
npm run tauri:build                  # 仓库根目录 = node scripts/build-desktop.mjs
# NSIS 工具链预置(Tauri 默认从 github.com 下载,本机不可达):
powershell -ExecutionPolicy Bypass -File packaging/fetch-tauri-nsis.ps1

# 实机冒烟（14 项）/ 服务控制冒烟（19 项）/ 安装包实测（26 项）
powershell -ExecutionPolicy Bypass -File packaging/smoke-test-tauri.ps1
powershell -ExecutionPolicy Bypass -File packaging/smoke-test-service-control.ps1
powershell -ExecutionPolicy Bypass -File packaging/verify-installer.ps1 -InstallerPath <exe>
```

## 两条边界（别踩）

1. **窗口控制不进 `window.wr`**：8 个 `desktopMethods` 是**跨运行时公共面**
   （Tauri / 浏览器同一份前端 bundle 都要认）；窗口控制是**壳层 chrome**（前端不参与窗口管理）。
   → 走 `window.__TAURI_INTERNALS__.invoke()` 直调 `wr_window_*`，
   **契约保持 8 项不变**，`validate-capabilities.mjs` 的四方一致校验不受影响。
2. **`system` 主题偏好下不强制窗口主题**：强制 `set_theme` 会把 webview 的
   `prefers-color-scheme` 钉死，系统主题在运行期切换就传不到前端 → `system` 必须放行。

## 环境要求

- **Rust**：GNU 工具链可构建与运行（开发机实测）；
  **打包（`tauri build`）需要 MSVC 工具链 + Windows SDK**（Tauri 官方要求）——
  开发机已装 VS Build Tools 2022 17.14.44 + MSVC 14.44 + Windows SDK 10.0.26100
  + `rustup target add x86_64-pc-windows-msvc`。**桌面壳的默认 target 就是 msvc**
  （`desktop/src-tauri/.cargo/config.toml` 的 `[build] target`，只作用于这个独立工程；后端 workspace 仍是 GNU 宿主）。
- **Tauri CLI**：装在 `desktop/`（`npm --prefix desktop install`）——
  CLI 以 cwd 为基准找 `src-tauri/`，装在仓库根会找不到配置。
- **WebView2 Runtime**：Windows 10/11 通常自带。缺失时 Tauri 会提示安装。
- **端口 3462 / 3460 必须空闲**：壳自己起后端并独占这两个端口。

## 已知限制

- `cargo test`（链接了 webview2 的目标）在 **GNU 工具链**下进程起不来
  （`WebView2Loader.dll` 动态装载 + harness 在 `target/<profile>/deps/`）→ 0xC0000135 / 0xC0000139。
  可执行文件本身不受影响。纯逻辑单测已拆到 `--test pure_logic` 规避；**msvc target 下不复现**。
  → 桌面壳测试记得带 `--target x86_64-pc-windows-msvc`。
- 8 个桌面方法中，更新三兄弟降级为只读（不接自动更新通道）。
- 最大化状态用 1s 轮询（壳层事件未接前端通道）—— 单窗口桌面应用，代价可接受。

参考：内部方案《Electron 迁移 Tauri 可行性与架构方案》（设计与决策，不随开源仓库发布）与 `docs/reference/02-架构设计.md`（当前事实）。
