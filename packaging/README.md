# WRouter 发布构建（Rust 单二进制交付）

- **Windows**：Tauri v2 桌面壳产出 NSIS 安装包（`WRouter_<ver>_x64-setup.exe`，15.4MB 量级；Electron 时代 154.6MB）。Windows 侧只有 `desktop/src-tauri/` 一条链。
- **Linux**：纯后端交付，无桌面壳，只发「单二进制 + 前端 SPA + systemd 用户单元」。

> **发布前必读（2026-11-07 破坏性变更）**：本版本只保证**最新数据格式**可用。旧配置键不再读取、旧 SQLite 内嵌正文/采集行不再并回、旧代理模式 `custom`/`system` 不迁移；不做自动迁移。发布说明必须显著写明「升级前备份数据目录；旧设置可能丢失、旧日志正文可能不可读；恢复建议为备份后重新配置」。详见 `README.md` 与 `docs/reference/04-功能与兼容性.md` §8。

发布目标：

- **Windows：Tauri NSIS `.exe`**（`WRouter_<ver>_x64-setup.exe`）—— 桌面壳 + 后端同进程，前端 SPA 与能力契约随包带出。
- **Linux：RPM（纯后端）** —— 单 musl 静态二进制 `wr-backend` + 前端 SPA + systemd 用户单元；
  **无 Electron / WebKitGTK / GPU / 沙箱 / FUSE**；体积从 ~100MB 降到 10MB 量级（0.2.3 为 9.57MB）。
- Linux NewStartOS / 麒麟 V10（glibc 2.28 基线）：RPM 安装后用 systemd 用户服务跑，
  管理面走浏览器 `http://127.0.0.1:3462/`；
  双轨产物另见 `backend/dist/wr-backend-<version>-<target>.tar.gz`（musl 主轨 / gnu.2.28 备选轨）。
- Docker：`packaging/Dockerfile.runtime` 多阶段构建（cargo-zigbuild 产 musl → alpine 基底），端口 C1 不变。

## 构建前提

- Node 22 + npm（vite、Tauri CLI）。
- **Windows 构建**：rustup（`backend/rust-toolchain.toml` 锁定 rustc 1.98.0）；windows-gnu 宿主另需
  WinLibs MinGW（dlltool/as，winget `BrechtSanders.WinLibs.POSIX.UCRT`，脚本自动发现）。
- **Linux 构建**：宿主侧 rustup + cargo-zigbuild + zig（交叉编译 musl）；容器只需 rpmbuild
  （镜像 `wrouter-linux-builder:v2` 已内置 rpmbuild 4.14.3）。
  **构建镜像是跨应用共享资产 —— 禁止删除 / 裁剪 / 重建**（见 AGENTS.md）。

## 命令

```powershell
# Windows(NSIS):预置 NSIS 工具链 → tauri build --target x86_64-pc-windows-msvc
npm run build:win          # = node scripts/build.mjs --win
npm run tauri:build        # = node scripts/build-desktop.mjs(只出 Windows 安装包)
# NSIS 工具链预置(Tauri 默认从 github.com 下载,本机不可达):
powershell -File packaging/fetch-tauri-nsis.ps1

# Linux(rpm 纯后端):宿主 cargo-zigbuild 产 musl → 容器内 rpmbuild
npm run build:linux        # = node scripts/build.mjs --linux(Docker)
npm run build:rpm          # = node scripts/build.mjs --rpm(同 build:linux)
# 注:AppImage 已下线 —— 纯后端无对应形态

# 单独构建后端二进制(不打包安装器;Linux 打包链内部用)
bash packaging/build-backend.sh                  # Linux:musl 静态 → wr-backend
bash packaging/build-rust.sh --target gnu228     # glibc 2.28 备选轨归档
```

产物暂存约定：`backend/dist/electron/`（`wr-backend` + `build-info.json`）只服务 **Linux** 打包链
（装配进 `packaging/wrouter.spec`）。**Windows 侧不需要独立后端二进制** —— Tauri 壳把后端
当 path 依赖**同进程**跑；随包带出的是前端 SPA 与能力契约（`bundle.resources`）。

## Linux RPM 打包链路

```
宿主  node scripts/generate-build-info.mjs        → build/build-info.json
宿主  packaging/build-backend.sh                  → musl 静态 wr-backend (13.3MB)
宿主  npm --prefix frontend run build              → frontend/dist (5.75MB)
容器  装配 /tmp/stage (ext4)                       → wr-backend + frontend/ + 契约 + 单元 + 钩子
容器  rpmbuild -bb packaging/wrouter.spec          → wrouter-<ver>-1.el8.x86_64.rpm (~9.5MB)
```

要点：

- **不经 fpm/nfpm**（镜像内无 nfpm）→ 历史 `rpmbuild: undefined symbol: EVP_md2` 坑不复存在。
- **musl 静态二进制 → RPM 无 glibc 依赖**（`AutoReq: no` + 显式声明 `systemd` / `/bin/sh`；
  实际依赖只有 systemd、`/bin/sh` 与 rpmlib 系列）→ 麒麟 V10 / UOS / CentOS 7 等老系统均可安装。
- 构建在容器内 ext4(`/tmp`)进行 —— 规避 NTFS 挂载卷 rename 坑。
- **systemd 单元由 rpm `%files` 直接拥有**（`/usr/lib/systemd/user/wrouter.service`），
  卸载时随包删除；安装钩子只打印提示（root 上下文无用户会话，无法代用户 `systemctl --user`）。

安装后目录结构：

```
/usr/bin/wr-backend                                  # 单 musl 静态二进制
/usr/share/wrouter/frontend/                          # 前端 SPA(index.html + assets/)
/usr/share/wrouter/contracts/wrouter-capabilities.json  # 管理面能力契约
/usr/share/wrouter/build-info.json                    # 构建清单
/usr/lib/systemd/user/wrouter.service                 # systemd 用户单元
/usr/libexec/wrouter/{after-install,after-remove}.sh  # 安装/卸载钩子
```

## 冒烟

```bash
# Windows 桌面壳实机冒烟(14 项)/ 服务控制冒烟(19 项)
powershell -File packaging/smoke-test-tauri.ps1
powershell -File packaging/smoke-test-service-control.ps1

# Windows 安装包实测(26 项:静默安装 → 管理面可用 → 卸载清理 + 用户数据保留)
powershell -File packaging/verify-installer.ps1 -InstallerPath <exe>

# Linux RPM 冒烟(rockylinux:8 容器,零本机影响):
# 安装 → 文件清单核查(无 Electron/webkit/mesa)→ 单二进制静态性 → SPA/静态资源/SPA fallback/
# API 404 不被吞/session 令牌 → 卸载后单元与二进制随之清除 → 用户数据保留
bash packaging/smoke-test-linux.sh <dist-linux 目录>

# wrouter.service systemd 冒烟(Rocky 8 systemd PID1 容器):
# 启停/开机自启/杀进程自动重启(Restart=on-failure)/journalctl --user
# + 管理面 SPA 浏览器可达 + ~/.wrouter 数据目录
bash packaging/smoke-test-systemd.sh <wrouter-*.rpm>

# 网关单二进制四步冒烟(启动/流式转发/管理页/落库,glibc 2.28 门)
# 历史脚本 scripts/verify/archive/smoke.sh 已随归档清理移除;
# 网关链路回归以 `cargo test --workspace` + `python scripts/verify/replay_golden.py` 为准。

# Docker 运行时镜像
docker build -f packaging/Dockerfile.runtime -t wrouter-gateway:latest .
docker run --rm -p 3460:3460 -p 3462:3462 -v wrouter-data:/data wrouter-gateway:latest
```

## systemd 服务

`packaging/wrouter.service`（用户级单元）随 RPM 安装到 `/usr/lib/systemd/user/wrouter.service`，
ExecStart 指向 `/usr/bin/wr-backend serve --host 127.0.0.1 --data-dir %h/.wrouter --serve-ui`
（D4：只监听回环），并设置 `WROUTER_SERVE_UI=1` / `WROUTER_FRONTEND_DIR=/usr/share/wrouter/frontend`
/ `WROUTER_CAPABILITIES_FILE=/usr/share/wrouter/contracts/wrouter-capabilities.json`。

启用（在登录会话中执行）：

```bash
systemctl --user enable --now wrouter.service
```

管理面：`http://127.0.0.1:3462/`（浏览器）；网关面：`http://127.0.0.1:3460/`。
数据目录：`~/.wrouter/`（与手跑后端、systemd 服务共用）。
没有 systemd 时，直接执行 `/usr/bin/wr-backend serve` 仍可使用 3460 Gateway 和 3462 管理 API。
