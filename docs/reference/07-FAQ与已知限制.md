# 07 · FAQ 与已知限制

> 最后核验:2026-11-07。排障速查 + 已接受限制登记。Python 时代的历史 bug 记录已随重写消失,只保留仍适用的根因模式(完整历史见 git 与方案归档)。

## 1. 常见问题

### 1.1 管理界面打不开 / 401

| 检查 | 方法 |
|---|---|
| 后端活着吗 | `curl http://127.0.0.1:3462/api/v1/health`(免认证) |
| token 对吗 | 除 health/capabilities 外全部要求 `x-wrouter-web-auth` 头;token 在 `<data_dir>/auth_token` 文件,或被 `WROUTER_WEB_AUTH_TOKEN` 覆盖。浏览器直接打开 `http://127.0.0.1:3462/` 时,前端经同源 `/api/wr/session` 自动取令牌(仅 loopback 放行);非 loopback 用 `?wr_web_token=<token>` |
| 端口对吗 | 管理面 3462、网关 3460;占用排查 `netstat -ano \| findstr <port>`(Windows)/ `ss -ltnp`(Linux) |

> **网关面(3460)401 `authentication_error` 是另一回事**:数据面强制路由密钥,
> 请求需带 `x-api-key` 或 `Authorization: Bearer <路由密钥>`;密钥在管理面“路由密钥”页
> 创建/查看,首次使用会自动生成一条默认密钥(删光后不再自动重建)。详见 [03 §1](./03-网关协议与路由.md)。

### 1.2 后端进程没起来

- 开发态(Tauri 壳):后端**同进程**跑在壳内,不存在"后端进程没起来"这件事 —— 壳起不来才会看不到管理面;此时看终端里 `cargo run` 的报错。
- 纯后端形态(Linux):查 `journalctl --user -u wrouter`(systemd)或前台运行的 stdout;桌面形态查 `<日志目录>/backend.log`。确认 `/usr/bin/wr-backend` 存在且能在目标机启动(glibc 基线、依赖库)。
- `wr doctor` 子命令可做环境自检。

### 1.3 RPC 手工探测

```bash
curl -s -X POST http://127.0.0.1:3462/api/wr/rpc \
  -H "x-wrouter-web-auth: $TOKEN" -H "content-type: application/json" \
  -d '{"method":"getGatewayStatus","params":{}}'
```

方法白名单见 [02 §5](./02-架构设计.md);应用内可在 DevTools 用 `window.wr` 直接调。

### 1.4 界面小问题

- **原生 `<select>` 深色主题发白**:Chromium 行为,控件上加 `style={{ colorScheme: "light dark" }}`,option 用 `bg-background text-foreground`。
- **改了前端没生效**:dev 模式 vite HMR 即时生效;**Rust 侧改动需重跑 `npm run dev`**(后端与壳同进程,改动要重新编译);打包态确认 `frontend/dist` 是新构建(`tauri build` 的 beforeBuildCommand 总会重建前端)。
- **装好的程序白屏 / 404**:后端 `frontend_dir` 默认指向源码树 → 安装态必须由壳把 `WROUTER_FRONTEND_DIR` 指到随包资源(`<安装目录>\frontend`);`packaging/verify-resource-origin.ps1` 可复现这个检查。

### 1.5 Linux 侧没有 GUI(纯后端交付)

Linux 交付为纯后端(无桌面壳)—— 只发「单二进制 + 前端 SPA + systemd 用户单元」。
管理面用**浏览器**访问 `http://127.0.0.1:3462/`,网关面在 `http://127.0.0.1:3460/`。
安装后启用服务(在登录会话中执行):

```bash
systemctl --user enable --now wrouter.service
```

AppImage 已下线(纯后端无对应形态);安装包为 `wrouter-<ver>-1.el8.x86_64.rpm`。

### 1.5.1 怎么停止服务 / 暂停网关

管理面**设置 → 通用 → 服务控制**里有三个动作(所有运行时通用):

| 动作 | 效果 | 能再拉起来吗 |
|---|---|---|
| **暂停服务 / 恢复服务** | 只关**网关面 3460**(不转发、不占端口;客户端会 connection refused),**管理面 3462 一直在线** | 能 —— 点「恢复服务」即可 |
| **退出应用** | 仅桌面端:关窗口(后端与壳同进程,随之退出) | 能 —— 重开应用 |
| **停止服务** | 独立进程 / systemd 托管时:后端优雅停机并退出(退出码 0) | **浏览器拉不起进程** —— 按页面提示用 `systemctl --user start wrouter.service` 重启 |

- Linux systemd 交付形态下,「停止服务」= 服务真的停了(`Restart=on-failure` 不拉起退出码 0 的进程);页面会给出重启命令。
- **桌面端没有「停止服务」按钮**:后端与壳同进程,停后端只会让窗口变废 —— 关窗口即可。
- 命令行等价物:`systemctl --user stop wrouter.service`(停) / `systemctl --user start wrouter.service`(起)。
- **「正在停止」≠「已停止」(2026-11-03 修正)**:`stopService` 的回执只代表**请求已受理**。页面据此显示「正在停止」,直到管理面真的连不上才切「已停止」—— 受理与退出是两件事,别把前者当后者。
- **不用再手动关标签页**:停机时后端会主动结束管理面的 SSE 长连接并给每个客户端补发一帧 `service-stopping`;前端收到后取消订阅与自动重连。早期版本依赖「关掉所有管理页面标签页」才让进程退出,那是长连接钉死优雅停机所致,已修复(回归测试见 `backend/crates/wr-server/tests/stop_drain.rs`)。
- **页面报「WRouter web API failed with HTTP 502」(2026-11-04 修复)**:这是管理面(3462)连不上了 —— 502 来自前端的反代(vite dev proxy / nginx),不是后端业务报错。已定位过一次真实原因:管理面的「排空上限 5s」被套在了整个服务生命周期外面,于是管理面启动 5 秒后就被掐断,而网关面与进程还活着 → 页面全挂、进程看似正常。现已改为「上限只在优雅停机开始后计时」,并加了回归测试(`backend/crates/wr-server/tests/mgmt_lifetime.rs`)。桌面壳端也补了:后端线程一死,壳会弹窗告知并退出,不再留一个所有请求都 502 的空窗口。
  - **自查**:若又见到 502,先确认 3462 是否在监听(`Get-NetTCPConnection -State Listen | Where-Object { $_.LocalPort -eq 3462 }`)。**不在监听 = 管理面已死**,这时页面必然全部无数据;网关面(3460)还在不代表管理面活着。

### 1.6 Tauri 桌面壳(Windows)

- **怎么起**:`npm run dev`(vite dev server + Tauri 壳 + **同进程后端**);纯逻辑单测 `cd desktop/src-tauri && cargo test --target x86_64-pc-windows-msvc --test pure_logic`;实机冒烟 `powershell -ExecutionPolicy Bypass -File packaging/smoke-test-tauri.ps1`;服务控制冒烟 `packaging/smoke-test-service-control.ps1`;安装包实测 `packaging/verify-installer.ps1`。
- **和 Electron 的关系**:**Electron 壳已退场**(`electron/` 目录已删除),Windows 侧 Tauri 是唯一桌面壳,Linux 侧无桌面壳。
- **后端同进程**:Tauri 壳不 spawn `wr-backend` 子进程,后端跑在壳内的**独立线程 + 独立 tokio runtime**上(依赖 `wr-server` 的 path 依赖)。因此 Tauri dev 会**独占 3462/3460** → 别同时跑第二个壳实例。
- **端口 3462 被占用**:多半是另一个后端实例(systemd 服务 / 残留的壳进程)在跑。先停掉再起 Tauri。
- **dev 数据目录**:debug 构建下数据目录重定向到 `<repo>/.dev/user-data/data`,**不碰** `%APPDATA%\WRouter\data`;发布构建才用生产目录(见 01 §5)。
- **已知限制**:开发机**已装 MSVC 工具链**(VS Build Tools 2022 17.14.44 / MSVC 14.44 / Windows SDK 10.0.26100 / `rustup target add x86_64-pc-windows-msvc`)。WebView2 走**动态装载**,因此链接了 webview2 的 **test harness 二进制在 GNU 工具链下起不来**(0xC0000135 / 0xC0000139);**可执行文件本身不受影响**。纯逻辑单测已拆到独立测试目标 `--test pure_logic` 规避(16 例);**msvc target 下该问题不复现**。**桌面壳默认 target 就是 msvc**(`desktop/src-tauri/.cargo/config.toml`,只影响这个独立工程,后端 workspace 仍是 GNU 宿主)。
- **NSIS 工具链**:`tauri build` 要从 github.com 下载 NSIS(本机 github.com 不达)→ 先跑 `packaging/fetch-tauri-nsis.ps1`(走镜像 + SHA1 校验),之后 `tauri build` 全程离线。
- **自动更新**:不接更新通道,设置页只读展示「自动更新暂不可用」,升级需手动下载安装包。
- **窗口 URL 决策**:生产形态加载 `http://127.0.0.1:3462/`(后端静态托管同一份 `frontend/dist`,不用 `tauri://` 资产协议);dev 形态加载 vite dev server(HMR)。
- **Windows 自绘标题栏**:Tauri v2 无 `titleBarOverlay` 等价 API → 壳侧 `decorations(false)` 去系统标题栏,前端自绘 min/max/close 三键(28px,贴右边缘)。拖拽区用 `data-tauri-drag-region`(非 `-webkit-app-region`)。窗口层命令走 `window.__TAURI_INTERNALS__.invoke()` 直调,**不进 `window.wr`** —— 8 个 `desktopMethods` 契约保持不变。
- **开机自启**:配置落盘在后端 `AppConfig.launchAtLogin`;壳启动时对账 OS 登录项(`HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run`),前端改开关后立即同步。**注册表条目指向当前 exe** → dev 构建(`cargo run`)会指向 debug exe,正式安装后由安装版接管。

### 1.6.1 管理面 RPC 不通 / 白屏：壳 → 注入 → 桥 → 后端

排查链路固定为 **壳 → 注入 → 桥 → 后端**，逐层确认（与 §1.1 / §1.5.1 / §1.6 互补）：

1. **壳起来了吗** —— 看终端 `[tauri]` 前缀输出；`cargo run` 自己报错就是壳层问题（编译/依赖/端口）。
2. **后端起来了吗** —— 后端**同进程**跑在壳内的独立线程 + 独立 tokio runtime，没有独立后端进程。`curl http://127.0.0.1:3462/api/v1/health`（免认证）探活；起不来时终端打印「后端启动失败」或「后端启动超时(60s)」。
3. **桥接层注入了吗** —— DevTools 看 `window.wr` 是否存在、`window.wr.runtime === "desktop"`。`window.wr` 由 `desktop/src-tauri/src/bridge_core.rs::init_script()` 在页面脚本**之前**注入。
4. **业务方法补齐了吗** —— `window.wr` 只有 **8 个桌面方法**；业务方法由 `frontend/src/web-client-bridge.ts` **就地补齐 28 个**。某方法 `undefined` 是前端桥没补，不是后端问题。
5. **鉴权头对吗** —— 管理面除 `/api/v1/health`、`/api/v1/capabilities` 外都要求 `x-wrouter-web-auth`；token 在 `<data_dir>/auth_token`，或被 `WROUTER_WEB_AUTH_TOKEN` 覆盖（见 §1.1）。
6. **端口被占了吗** —— 壳独占 3462/3460，别同时跑第二个实例；`netstat -ano | findstr 3462`（见 §1.6）。
7. **白屏 / 404（仅安装态）** —— 后端 `frontend_dir` 默认指向源码树，干净机器上没有 → 壳必须把 `WROUTER_FRONTEND_DIR` / `WROUTER_CAPABILITIES_FILE` 指到随包资源（见 §1.4、06 §7）。
8. **页面报「WRouter web API failed with HTTP 502」** —— 502 来自反代（vite proxy / nginx），表示管理面 3462 连不上。**不在监听 = 管理面已死**，网关面 3460 还活着不代表管理面活着（详见 §1.5.1）。dev 模式可用 `http://localhost:<vite 端口>/api/wr/rpc` 直连代理链验证。

### 1.7 通用排障五步

1. curl 直探 RPC/health,排除前端因素;
2. DevTools 检查 `window.wr` 与网络面板请求头(token);
3. netstat/ss 查端口占用;
4. `sqlite3 config.sqlite` 直查数据(表清单见 02 §4);
5. 核对前后端字段名拼写(RPC 方法名/参数名大小写敏感)。

## 2. 已接受限制与技术债登记

状态:Open(待处理)/ Accepted(接受现状)/ Watch(实际使用中观察)。

| # | 项 | 状态 | 说明 |
|---|---|---|---|
| TD-1 | 代码签名未启用 | Accepted | Windows 未签 Authenticode、Linux 未签 GPG;Windows 侧签名走 `tauri.conf.json` 的 `bundle.windows.certificateThumbprint`。分发时会有 SmartScreen/来源告警 |
| TD-2 | 无自动更新通道 | Accepted | 不接自动更新(无 updater 插件、无 feed)。保留 `getUpdateStatus` RPC,设置页只读展示「自动更新暂不可用」,升级靠手动下载安装包 |
| TD-3 | macOS 发布轨缺失 | Accepted | 无构建目标与验证;`build.mac` 仅为配置占位 |
| TD-4 | systemd 服务文件打包链路 | Closed | RPM `%files` 直接拥有 `/usr/lib/systemd/user/wrouter.service` → 卸载随包删除,不再经 extraResources 传递(风险消失) |
| TD-5 | NewStartOS / 麒麟 V10 实机验收 | Closed | Linux 交付改为纯后端后,原 GPU/沙箱/FUSE/WebKitGTK 限制不再适用(无 GUI 依赖);容器 glibc 2.28 基线 + RPM/systemd 两层冒烟已过,残留的实机差异(字体/输入法)随用户真机验收一并确认 |
| TD-6 | 模型改名后历史用量分裂 | Watch | "名字即键",无稳定 model_id(方案归档原 09 讨论未拍板);影响面分析保留在归档 |
| TD-7 | ready file 临时权限 | Watch | ready file 写在系统临时目录,多用户机器上理论上可被本机其他用户读到内容(不含密钥) |
| TD-8 | 大日志正文捕获的磁盘占用 | Watch | 正文默认关闭;开启后 7 天自动过期,但高流量下 7 天正文仍可观 |
| TD-9 | Linux systemd 服务链路真机验收 | Open | 「停止服务」→ `systemctl --user status` 显示 inactive → `systemctl --user start` 恢复 的完整链路尚未在 Linux 机器上跑过(容器内只验到 `managedBy=process` 语义)。2026-11-03 修复了「点停止服务后进程不退出」(SSE 长连接钉死优雅停机 + 网关暂停态收不到停机信号),该链路已由 `backend/crates/wr-server/tests/stop_drain.rs` 5 条自动化覆盖(含「SSE 长连接存在时仍能退出」「网关暂停态仍能退出」),但 **systemd 真机**的 `status → inactive` 仍待用户侧确认 |
| TD-10 | Windows v0.2.3 真机安装验收 | Open | 自动化已过(安装包 26/26 + 服务控制 19/19),但**用户侧安装与日常使用验收**尚未完成 |
| TD-11 | 未启用 `--disable-gpu` | Accepted | 实测:Σ独占内存 260.2 → 194.0MB(-25%),GPU 进程 priv 86.0 → 18.8MB,视觉差异 0.25%,动画 -2.8%;已评估落地方式(Tauri v2 无 `additionalBrowserArgs` 配置项,须用 `WebviewWindowBuilder::additional_browser_args()` 且**整体替换** wry 默认的 `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`,仅 Windows 生效),用户裁决暂不落地 |
| TD-12 | 旧数据格式不兼容 | Accepted | 只保证**最新数据格式**可用:旧配置键不再读取、旧 SQLite 内嵌正文/采集行不再并回、旧代理模式不迁移;不自动迁移、不删除历史行。**升级前必须备份数据目录**;旧设置可能丢失、旧日志正文可能不可读。详见 [04 §8](./04-功能与兼容性.md) |
| TD-13 | OAuth / RBAC 未实现 | Accepted | 无 OAuth 账户连接器、无多用户/RBAC;代码签名/自动更新/macOS 发布轨另见 TD-1/2/3 |
| TD-14 | 从旧 Electron 版升级需先卸载 | Accepted | 否则新壳装进旧安装目录、旧 Electron 文件残留；NSIS 把上次安装目录记在 `HKCU\Software\wrouter\WRouter`，卸载器**不删**该键 |

## 3. 快速链接

- 端口/数据目录/术语 → [01-项目概览](./01-项目概览.md)
- 请求不出去/路由不符预期 → [03-网关协议与路由](./03-网关协议与路由.md)
- 密钥/日志策略/上游代理 → [04-功能与兼容性](./04-功能与兼容性.md)
- 关键机制怎么实现（协议/吞吐/选路/韧性/生命周期/指纹/缓存）→ [../design/](../design/README.md)
- 打包故障 → [06-构建与发布](./06-构建与发布.md) §7
- 历史方案与决策 → [../planning/archive/](../planning/archive/README.md)
