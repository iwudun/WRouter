# backend — WRouter Rust 后端

方案 #14(前后端大改造)的 Rust 重写工作区,R7(2026-08-30)起为仓库唯一后端
(原 Python `backend/` 已退役,行为对齐基准:`../testdata/golden/` 黄金样本与
git 历史中的 Python 实现;目录名 `backend` 由本 workspace 于 R7.10 接替)。

## 分层与依赖方向(方案 #14 D14)

```
             wr-cli(bin: wr,入口装配)
                 │
             wr-server ──────────┐
             │    (axum 装配/运行时/日志/Settings)   │
             ▼                    ▼
         wr-app              wr-infra
   (应用服务 + 端口 trait)   (C9 配置读取 / R4 sqlite / R2 上游客户端)
             │                    │
             └────► wr-domain ◄───┘
              (纯领域:禁 async 运行时 / Web / HTTP / SQLite)
```

- `wr-test-utils`:跨 crate 测试基建,**仅允许作 dev-dependency**。
- 端口 trait 的 async 取舍(R1.1 决策):全仓统一 `async-trait` 宏,
  保证 dyn 对象安全以支持 wr-server 装配注入与测试替身;不使用裸
  RPITIT。所有第三方版本统一在根 `Cargo.toml` 的
  `[workspace.dependencies]` 管理,防版本漂移。

## 开发环境搭建(30 分钟路径)

1. **rustup + rustc 1.98.0**:`rust-toolchain.toml` 已锁定,进入本目录后
   rustup 自动安装对应 toolchain(minimal + clippy + rustfmt)。
2. **Windows 宿主追加**:WinLibs MinGW(winget
   `BrechtSanders.WinLibs.POSIX.UCRT`),并把其 `bin` 目录加入 PATH——
   `windows-sys` 的 build script 需要找到 `dlltool.exe` 与 `as`。
3. **Linux 交叉轨追加**(仅交付构建需要):zig(winget `zig.zig`)与
   `cargo install cargo-zigbuild --locked`。
4. **验证**:`scripts/check-rust.sh` 全绿即环境就绪。

```bash
cargo run -p wr-cli -- doctor   # 有效装配诊断
cargo run -p wr-cli -- serve    # 启动管理面 3462 + 网关面 3460
curl http://127.0.0.1:3460/health
```

前端/桌面壳联调:仓库根 `npm run dev`(vite + Tauri 壳,后端同进程;旧文档描述的
`backend/target/{debug,release}/wr` 并注入 `WROUTER_BACKEND_BIN`)。

## 工具链版本(R1.7 锁定,保证产物可复现)

| 组件 | 版本 | 锁定方式 |
|------|------|---------|
| rustc / cargo | 1.98.0(minimal profile) | `rust-toolchain.toml`(rustup 自动切换) |
| clippy / rustfmt | 随 rustc | `rust-toolchain.toml` components |
| zig | 0.16.0 | winget(`zig.zig`);升级需重跑 musl/gnu228 产物验证 |
| cargo-zigbuild | 0.23.3 | `cargo install cargo-zigbuild --locked` |
| MinGW binutils(dlltool/as) | WinLibs 16.1.0(GCC 14) | winget `BrechtSanders.WinLibs.POSIX.UCRT`;仅 windows-gnu 宿主需要 |

## 本地命令

```bash
# 质量门(R1.10 / §5.2):纯度守护 + fmt + clippy -D warnings + test
scripts/check-rust.sh

# 直接跑
cd backend
cargo test --workspace
cargo run -p wr-cli -- version          # wr version
cargo run -p wr-cli -- doctor           # 有效装配诊断
cargo run -p wr-cli -- serve            # 启动管理面 3462 + 网关面 3460
WR_VERSION=9.9.9 cargo build -p wr-cli  # 版本注入验证(R1.5)

# Linux 交付双轨(D5,交叉编译)
packaging/build-rust.sh                    # musl 静态主交付轨
packaging/build-rust.sh --target gnu228    # glibc 2.28 动态备选轨
packaging/build-rust.sh --target host      # 仅宿主 release,不归档

# 黄金样本回放(行为回归;一条命令:构建→mock→22 case diff)
python scripts/verify/replay_golden.py          # 默认用 target/replay 构建并自动拉起 mock
python scripts/verify/replay_golden.py \
  --command "<abs-path>/backend/target/release/wr.exe serve --data-dir {data_dir} --ready-file {ready_file}"
```

## 版本注入(R1.5)

运行时版本唯一来源是根 `package.json` 的 `version`。构建脚本读取后以
`WR_VERSION` 环境变量注入,`wr-server::VERSION`(`option_env!`)在
未注入时回退 `"0.0.0-dev"`。cargo 侧 package version 恒为 `0.0.0`,
不作为对外版本(与「唯一手改根 package.json」规则一致)。

## 交付产物(D5 双轨)

`packaging/build-rust.sh` 产出归档到 `backend/dist/`:

| 轨 | target | 形态 | 用途 |
|----|--------|------|------|
| musl(主) | `x86_64-unknown-linux-musl` | 静态单二进制 | glibc 2.28 目标机(麒麟 V10 / Rocky 8)默认交付 |
| gnu228(备) | `x86_64-unknown-linux-gnu.2.28` | glibc 2.28 动态 | musl DNS/nsswitch 企业内网场景兜底 |
| host | 当前宿主 | — | 本机开发 |

归档命名 `wr-backend-<version>-<target>.tar.gz`,附 `.sha256`。

## 测试布局约定(R1.9)

| 层级 | 位置 | 说明 |
|------|------|------|
| 单元测试 | 各 crate 内 `#[cfg(test)] mod tests` | 纯逻辑,无 IO |
| 兼容/集成测试 | `crate/tests/*.rs` + `tests/fixtures/` | 如 `wr-infra/tests/config_compat.rs`(C9 配置样本) |
| e2e / parity | `wr-server/tests/` | `golden_path.rs` 钉住 `testdata/golden/` 相对可达;22 例黄金回放工具在 `scripts/verify/replay_golden.py`(定义见 `scripts/verify/cases.py`) |
| 跨 crate 基建 | `wr-test-utils`(dev-dep) | 真实 socket 启停、空闲端口 |

流式/SSE 相关测试一律走真实 socket(`wr-test-utils::net`),不用内存
传输(R0 教训:内存传输测不出 chunk 边界)。

## 故障排查

| 现象 | 根因 | 处理 |
|------|------|------|
| `CreateProcess` 失败,build script 找不到 dlltool | windows-gnu 宿主缺 WinLibs MinGW 或其 bin 不在 PATH | 安装 WinLibs 并手工加入 PATH(见上);rustup 侧路径 `check-rust.sh` 已自动处理 |
| `wr version` 显示 `0.0.0-dev` | 构建时未注入 `WR_VERSION` | 经 `packaging/build-rust.sh` 构建,或手动 `WR_VERSION=$(node -p "require('./package.json').version") cargo build -p wr-cli`;**桌面壳同理**(壳启动时也要注入,否则设置页版本号是 `0.0.0-dev`) |
| 开发态(`npm run dev`)设置页「当前版本」是 `0.0.0-dev` | `scripts/dev.mjs` 漏注入 `WR_VERSION` → 编译期常量 `option_env!("WR_VERSION")` 取到 `None` | 已修:`dev.mjs` 从根 package.json 读版本并在启动 cargo 前注入。**新增 dev/构建入口时必须在编译 wr-server 前注入**(`scripts/sync-version.mjs --check` 会守门,漏注入即退出码 1) |
| 改了版本号但产物没变 | `WR_VERSION` 是 env 编译参数,值不变则增量编译不重跑 | 触发重编:`cargo clean -p wr-cli` 或改一下 `WR_VERSION`(见 R1.5 陷阱记录) |
| tracing 日志挤在同一行 | 自定义 formatter 的换行处理 | 保持 `wr-server/src/logging.rs` 现有 formatter;换行符必须显式写,继承默认 formatter 会丢 |
| 回放 diff:usage 字段不一致 | 多命名 usage 归一/合并语义 | 见 `wr-domain/src/usage.rs`(取 max 合并);不要在 relay 层特判 |
| musl 产物域名解析失败 | musl 直查 DNS、无 nsswitch | 企业内网场景改用 gnu228 备选轨(D5) |

## 约束与守护

- C1-C10 行为约束清单见内部设计记录《前后端大改造实施方案》§6（不随开源仓库发布）。
- `wr-domain` 的 Cargo.toml 由 `scripts/check-rust.sh` 强制纯度 grep,
  新增依赖前先确认不属于禁用类别(tokio/axum/reqwest/rusqlite/tower)。
- 质量门四步(§5.2,R7 收拢版):`cargo test`(含 fmt/clippy,`scripts/check-rust.sh`)/
  tsc + vitest / `node scripts/validate-capabilities.mjs`。
