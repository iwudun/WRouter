# LESSONS —— 仓库内踩坑活文档

> **性质**：对**任何 agent 与人**都可见的踩坑知识库。每条教训 = **规则 + why + 证据（seen 次数 / 首次出现日）**。
> **来源**：内部方案《magpie 借鉴优化实施方案》阶段一～五的实施记录、本地门禁实践，以及 2026-11-05 补做 §7「贯穿工程实践」时的实测。
> **维护规则**：
>
> 1. 每次实施方案的「坑」段落**顺手追加**到本文件，不必另起文档；
> 2. 同一条教训在 **3 个不同日子**重复出现 → **晋升**进 [`AGENTS.md`](../AGENTS.md)（工程约定）或 [`docs/reference/05-开发指南.md`](reference/05-开发指南.md) §8 高频陷阱（用户可见的坑），本文件标注「已晋升」并保留原文；
> 3. 只在**规则层面**成立的才写进来 —— 一次性事故写进对应方案文档的实施记录即可，不要污染本文件；
> 4. `seen` 只统计**不同日子**的重复（magpie 口径）；同一天撞两次不计数。

---

## 索引

| ID | 教训 | seen | 状态 |
|---|---|---|---|
| [L-001](#l-001) | rustfmt 单文件检查会连带整个模块树 | 2 | 已晋升 |
| [L-002](#l-002) | 统计 rustfmt/clippy 输出必须显式 UTF-8 读 | 2 | 候选（再 1 次即晋升） |
| [L-003](#l-003) | clippy 增量判定要看告警指向哪个文件 | 2 | 候选 |
| [L-004](#l-004) | 前端门禁禁「cd + 输出重定向落盘」 | 1 | — |
| [L-005](#l-005) | 原位互换别用 `git checkout --` | 1 | — |
| [L-006](#l-006) | Windows 下 `rm` 是 `Remove-Item` 别名 | 2 | 候选 |
| [L-007](#l-007) | 质量门在 master 上既有红是基线事实 | 2 | 已晋升 |
| [L-008](#l-008) | 改了后端 crate 不等于重链了 `wr.exe` | 1 | — |
| [L-009](#l-009) | 用编辑工具写中文后必须扫 U+FFFD | 1 | — |
| [L-010](#l-010) | PowerShell 5.1 的落盘与读取编码 | 2 | 候选 |
| [L-011](#l-011) | `.ps1` 要 BOM+CRLF，容器内 `.sh` 要 LF | 1 | 已晋升 |
| [L-012](#l-012) | 内嵌 TCP mock：请求体可能与请求头同包到达 | 1 | — |
| [L-013](#l-013) | 制造「传输中断」的最确定构造 | 1 | — |
| [L-014](#l-014) | 红测即 bug —— 但先核对量纲 | 1 | — |
| [L-015](#l-015) | `%TEMP%` 残留目录造成 schema 测试假红 | 1 | — |
| [L-016](#l-016) | 时间类测试两边留余量 | 0 | — |
| [L-017](#l-017) | Tailwind v4 裸 `border` 落到 currentColor | 1 | 已晋升 |
| [L-018](#l-018) | Tailwind v4 preflight `svg { display: block }` | 1 | 已晋升 |
| [L-019](#l-019) | 浮层被祖先 `overflow` 裁掉 | 1 | 已晋升 |
| [L-020](#l-020) | 前端不认识后端合法取值就回落默认值 | 1 | — |
| [L-021](#l-021) | 字体 preload 的挑选与 crossorigin 剥离是耦合的 | 1 | — |
| [L-022](#l-022) | 修一处要遍历同族 | 1 | — |
| [L-023](#l-023) | 配置项不是轻量改动 | 1 | — |
| [L-024](#l-024) | 语义泵重建客户端流时，终止字段整条流只能有一个 | 1 | — |
| [L-025](#l-025) | 黄金基线会把既有缺陷固化下来 | 1 | — |
| [L-026](#l-026) | 编译期常量 `option_env!` 的每个编译入口都要注入 | 1 | — |
| [L-027](#l-027) | 门禁红在早期 crate 时，后续 crate 的 lint 永远没被检查过 | 1 | — |
| [L-028](#l-028) | 归一规则必须幂等，否则历史数据与新数据对不上 | 1 | — |
| [L-029](#l-029) | 收口帧不是内容帧：done 事件只收口、不重发 | 1 | — |
| [L-030](#l-030) | 分析记录里的「根因编号」不等于实际可触发的路径 | 1 | — |
| [L-031](#l-031) | 缓存值必须与读取路径的表示一致，有状态引用不进缓存 | 1 | — |
| [L-032](#l-032) | Windows 上 `child.kill()` 杀不掉 npx 底下的真实进程 | 1 | — |
| [L-033](#l-033) | 集成测试共用固定临时目录，并行时撞 SQLite 锁 | 1 | — |

---

## A · 门禁与工具链

### L-001

**rustfmt 单文件检查会连带整个模块树**

- **规则**：`rustfmt --check <某个文件>.rs` **不能**用来判断「这个文件有没有格式漂移」—— 它顺着 crate 根的 `mod` 声明把整个模块树一起格式化，输出里混进大量无关文件的 hunk。判定增量漂移必须**原位互换**：备份当前版本 → 覆盖基线版本 → 跑同一条命令 → 还原 → SHA256 校验还原无误。
- **why**：把文件抽到临时目录单独跑，模块树丢了 → rustfmt 不再跟进 `mod` → 得到**假的「0 漂移」**结论。
- **证据**：seen 2（2026-11-04 阶段四、2026-11-05 阶段五）。**已晋升** → `AGENTS.md` §质量门在 master 上既有红。
- **应用**：任何 Rust 仓库的 fmt 增量判定；同理适用于 clippy。

### L-002

**统计 rustfmt/clippy 输出必须显式 UTF-8 读**

- **规则**：读门禁输出文件时必须 `Get-Content -Encoding UTF8`；统计 hunk 数按行数 `^Diff in `，别把整段拼成单字符串再计数（会恒为 1）。
- **why**：PowerShell 5.1 默认按 **GBK** 解码 → 中文行被解码坏 → **相邻行粘连** → `Diff in` 标记落到行首之外被漏计。同一份输出，默认编码统计得 334/338 hunk、per-file 3/4；UTF-8 下真实值是 362/362、per-file 5/7。**用错编码会同时低估总数和单文件数** → 增量判定结论直接错。
- **证据**：seen 2（2026-11-04 阶段四、2026-11-05 阶段五）。
- **应用**：所有「统计门禁输出再对比基线」的动作。

### L-003

**clippy 增量判定要看告警指向哪个文件**

- **规则**：判定「本次是否新增 clippy 告警」要比**两栏**：告警总数 + 逐文件条数。两栏都一致才算零新增。
- **why**：只看总数会把「A 文件少 1 条、B 文件多 1 条」误判成没变；而 `-D warnings` 下**新增文件**自身必须零漂移。
- **证据**：seen 2（2026-11-04 阶段四、2026-11-05 阶段五）。
- **应用**：后端改动收尾。

### L-004

**前端门禁禁「cd + 输出重定向落盘」**

- **规则**：跑前端门禁用 `cmd /c "cd /d <dir> && npx vitest run 2>&1" | Select-Object -Last N`（重定向留在 cmd 内部、**不落盘**），不要用「cd 进子目录再把输出重定向到文件」。
- **why**：本地硬安全策略会拦「命令切换工作目录 + 写/删目标」的组合，报 `write/delete targets cannot be verified safely after a command working directory change`；落盘重定向同样触发，不只是写源文件。
- **证据**：seen 1（2026-11-05 阶段五）。
- **应用**：前端 vitest / tsc / vite 构建。后端 cargo 命令若工作目录不变，落盘仍可用。

### L-005

**原位互换别用 `git checkout --`**

- **规则**：做基线对比时用 `Copy-Item` 备份 → 覆盖基线版本 → 跑命令 → `Copy-Item` 还原 → SHA256 比对；**不要用 `git checkout -- <file>`**。
- **why**：本地硬安全策略会拦「工作目录切换 + 写/删目标」的组合，`git checkout --` 正好命中，报 `recursive write/delete target cannot be verified inside the workspace`。
- **证据**：seen 1（2026-11-05 阶段五）。同一次拦截也会打到「用 bash 直接改源文件」→ 改源码用编辑工具，别硬怼 shell。
- **应用**：L-001 / L-003 的原位互换流程。

### L-006

**Windows 下 `rm` 是 `Remove-Item` 别名**

- **规则**：删除文件/目录走 `& "<activeDataDir>\bin\mavis-trash.cmd" "<绝对路径>"`（移入回收站，可恢复）；**不要用 `rm`/`Remove-Item`**。
- **why**：`rm` 在 PowerShell 里是 `Remove-Item` 的别名 → 触发本地硬安全策略的「永久删除禁止」拦截。`mavis-trash.cmd` 的目标直接跟在命令后、**不带 `--`**（`--help` 会被当成相对路径解析而报错）。
- **证据**：seen 2（2026-10、2026-11-05）。
- **应用**：任何需要清理临时文件/目录的动作。

### L-007

**质量门在 master 上既有红是基线事实**

- **规则**：`cargo fmt --check` / `cargo clippy -D warnings` / `npm run typecheck:unused` 在 master 上**本来就有红**（rustfmt/clippy 版本差异、既有的未用符号）。**不要全仓格式化、不要顺手清既有告警** —— 那会改动上百个无关文件。
- **why**：既有红不是本次引入的回归；「顺手修」会把一次功能改动变成一次全仓重排，评审成本与回滚成本都不可控。
- **证据**：seen 2（2026-11-04 阶段四、2026-11-05 阶段五；基线 525 hunk / 80 文件、clippy 31 处 / 14 文件）。
- **晋升**：`AGENTS.md` §质量门在 master 上既有红。
- **应用**：每次后端改动的收尾。

### L-008

**改了后端 crate 不等于重链了 `wr.exe`**

- **规则**：要重链最终的 `wr.exe` 必须构建 **`-p wr-cli`**；只构建 `wr-backend` 之类的子 crate 不会更新最终可执行文件。
- **why**：可执行文件由 `wr-cli` 产出，子 crate 只是依赖 —— 只编依赖不编入口，产物仍是旧的。
- **证据**：seen 1（2026-10）。
- **应用**：本地验证「改动是否真的进了可执行文件」。

### L-026

**编译期常量 `option_env!` 的每个编译入口都要注入**

- **规则**：用 `option_env!("WR_VERSION")` 这类**编译期**常量承载版本号时，**每一个会编译该 crate 的入口**（dev 编排脚本、各打包脚本、容器构建）都必须在编译前注入对应环境变量，且这些注入点要**逐个守门**（`scripts/sync-version.mjs --check` 逐个检查注入语句还在不在）。运行期再设环境变量**无效** —— 值在 rustc 编译那一步就固化了。
- **why**：2026-11-05 —— 开发态设置页「当前版本」长期显示 `0.0.0-dev`。根因是 `scripts/dev.mjs` 启动 `cargo run` 时没注入 `WR_VERSION`，编译 `wr-server` 时 `option_env!` 取到 `None` → 回退硬编码占位符。发布链路（`packaging/build-rust.sh` / `scripts/build-desktop.mjs`）注入正常，所以**只看发布产物发现不了**，只有开发态可见 —— 而且这类缺陷是**静默**的：不报错、不崩，只是数字不对。（cargo 会把 `option_env!` 记成 `# env-dep:WR_VERSION` 写进 `target/**/wr_server-*.d`，版本值一变就自动重编该 crate，不需 `cargo clean`。）
- **证据**：seen 1（2026-11-05，开发态设置页显示 `0.0.0-dev`）。
- **应用**：任何「编译期注入型常量」（版本号、构建渠道、commit）的注入链路；新增编译入口时同步补注入 + 守门条目。

### L-027

**门禁红在早期 crate 时，后续 crate 的 lint 永远没被检查过**

- **规则**：质量门（`cargo clippy --workspace --all-targets -- -D warnings`）里若**前面的 crate 先失败**，cargo 会中止整个编译 —— 排在后面的 crate 的 lint **一次都没跑过**。所以「门禁绿/红」不能代表后面 crate 的 lint 状态；要检查后段 crate 的 lint，得单独跑 `cargo check -p <crate> --all-targets`。
- **why**：2026-11-05 —— `wr-domain` 的 clippy 常年 28~30 个 error，门禁永远在这一步就断。排在后面的 `wr-server` / `wr-infra` 因此带着 5 条 rustc warning（死变量、dead_code）长期无人发现，**只在开发态 `npm run dev` 的终端里冒出来**。另一层：`cargo build`（只编 lib 目标）与 `--all-targets`（含测试目标）的 warning 集合**不同** —— 只被同文件单测用到的私有方法在 clippy 下不算 dead_code，但 `cargo build` 会报。所以「dev 终端看到的 dead_code」在门禁输出里根本不会出现，两边都要看。
- **证据**：seen 1（2026-11-05，dev 终端 5 条 warning；`cargo clippy --workspace --all-targets` 在 `wr-domain` 即中止，`cargo check --workspace --all-targets` 才暴露全量）。
- **应用**：判断「某 crate 的 lint 状态」时；清理开发态终端噪音时；任何「门禁红了但报错位置与改动无关」的排查。

---

## B · 编码与文本

### L-009

**用编辑工具写中文后必须扫 U+FFFD**

- **规则**：往源文件写中文注释后，**必须**扫一遍 U+FFFD（Unicode 替换字符，UTF-8 解码失败时的兜底字符）—— 编辑工具在写入含中文的替换文本时可能把个别字符写坏。
- **why**：损坏是**静默**的：编译通过、测试通过，只有肉眼读注释才发现（2026-11-05 实测：「绑定」的第二个字被写成了 U+FFFD）。
- **证据**：seen 1（2026-11-05 阶段五，写注释时「绑定」损坏）。
- **应用**：任何含中文的源码/文档写入。

### L-010

**PowerShell 5.1 的落盘与读取编码**

- **规则**：捕获命令输出落盘用 `cmd /c "... > file 2>&1"`（`cmd` 写出的字节 rustfmt/node 读得了）；**不要用 PowerShell 的 `>` 重定向**（PS 5.1 写 UTF-16LE，rustfmt/node 读不了）。读取时显式 `-Encoding UTF8`（见 L-002）。
- **why**：PS 5.1 的 `>` 产出 UTF-16LE 带 BOM，后续统计脚本按行匹配 `Diff in` / `--> ` 全部落空 → 误判「零告警」。
- **证据**：seen 2（2026-11-04 阶段四、2026-11-05 阶段五）。
- **应用**：门禁输出落盘与统计。

### L-011

**`.ps1` 要 BOM+CRLF，容器内 `.sh` 要 LF**

- **规则**：`.ps1` 必须存 **UTF-8 with BOM + CRLF**（PowerShell 5.1 对 LF 中文注释按 GBK 解析会炸）；容器内执行的 `.sh` 必须是 **LF**。
- **why**：Windows 的 `WriteAllLines` 默认写 CRLF → 容器内 bash 报 `set: pipefail: invalid option name`（**报错信息极具误导性**，实际是行尾问题）。
- **证据**：seen 1（2026-10）。
- **晋升**：`AGENTS.md` §行尾与脚本约定。
- **应用**：写脚本、提交前 `git ls-files --eol` 自查。

---

## C · 测试与集成

### L-012

**内嵌 TCP mock：请求体可能与请求头同包到达**

- **规则**：读请求体要**从已读缓冲里按 `\r\n\r\n` 切出 body**，不要再单独循环读 `content-length`。
- **why**：请求体可能与请求头**同包到达**。若先读完头就另起一轮读 body，会一直等一个**已经到齐**的 body → mock 永不响应 → 网关只等到读超时，表现为「upstream read timed out」而不是断流，排查方向被带偏。
- **证据**：seen 1（2026-11-05 阶段五的断流续写端到端用例）。
- **应用**：任何用内嵌 TCP 上游构造请求的集成测试。

### L-013

**制造「传输中断」的最确定构造**

- **规则**：用「**Content-Length 声明远大于实际内容 + 写完即 `shutdown(Both)` + drop**」制造传输中断；**不要**用「chunked 缺终止符」。
- **why**：chunked 缺终止符在 Windows 上可能挂到超时（而不是立刻断），测出来的行为不稳定。
- **证据**：seen 1（2026-11-05 阶段五）。
- **应用**：断流 / 半截响应的测试构造。

### L-014

**红测即 bug —— 但先核对量纲**

- **规则**：测试变红时，**先核对测试自己的期望值**（单位、量纲、时区），再改实现。
- **why**：2026-11-05 补 C-9 单测时，`formatQuotaWindowDuration(90_000)` 期望 `"90m"` 变红 —— 但 90 000 ms 是 **90 秒**不是 90 分钟，实现是对的、测试写错了。红测是信号，不是判决。
- **证据**：seen 1（2026-11-05）。
- **应用**：所有「测试红了 → 改实现」的动作。

### L-015

**`%TEMP%` 残留目录造成 schema 测试假红**

- **规则**：`cargo test -p wr-infra` 里 `sqlite::schema::tests::bootstrap_accepts_legacy_snapshot_db_verbatim` 偶发报 `table account_snapshots already exists` 时，**先重跑一次**再判定回归。
- **why**：该测试的临时目录用 `std::process::id()` 当唯一键且**从不清理** → Windows 复用 PID → 新一轮测试拿到旧 PID 就撞上残留库。
- **证据**：seen 1（2026-11-04；残留库创建于 08:33，失败发生在 21:10，PID 跨 13 小时被复用）。
- **应用**：wr-infra 的 schema 测试；该失败只在 wr-infra 出现，`git diff -- backend/crates/wr-infra` 为空即可排除自身改动。

### L-016

**时间类测试两边留余量**

- **规则**：涉及时间/窗口的断言，**上下界都留余量**（不要断言到毫秒级相等）。
- **why**：CI 与本机负载不同，边界上的等值断言会在别人机器上偶发失败，而这种失败无法复现、无法定位。
- **证据**：seen 0（来自 magpie 的核心十条，本仓库尚未实际踩到；先立规则）。
- **应用**：任何 `now` / 窗口 / TTL 相关断言。

### L-025

**黄金基线会把既有缺陷固化下来**

- **规则**：基线红了先判「本次回归」还是「基线里本来就有的错」。**基线不是真理** —— 它可能是既有缺陷被录进去的。重采（把新行为录成基线）之前必须**人工过一遍旧基线内容**，问一句「这个旧行为本身对吗」。采集脚本的纪律与它同源：`scripts/verify/README.md` 的「补样本前先人工过一遍基线」。
- **why**：2026-11-05 —— `openai-chat-stream-tool-first-fragment` 的黄金基线里本来就写着两个 `finish_reason`（上游的 `tool_calls` + 网关补的 `stop`）。重采它等于把「两个终止原因」这个缺陷**固化成「正确行为」**，此后谁再修这个 case 都会被基线挡回来。单测里还另有一条 `chat_client_rebuilds_envelope_and_double_finish` 把双块行为写成了断言 —— **缺陷被基线 + 单测双重锁死**。
- **证据**：seen 1（2026-11-05，Qoder CN 经 WRouter 调 DeepSeek 工具调用报 `conflicting_finish_reason`）。
- **应用**：任何「基线红了 → 重采」的场合；新增黄金 case 时人工核对内容；发现基线与单测同时锁住某个行为时，先问「这个行为对吗」。

---

## D · 前端与 UI

### L-017

**Tailwind v4 裸 `border` 落到 currentColor**

- **规则**：写 `border` 一定要同时写颜色类（如 `border border-border`），全仓统一用 `--border` token，不要写死 `white` / 具体色值。
- **why**：Tailwind v4 的裸 `border` 只输出 `border-style` + `border-width`，**不输出 `border-color`** → 颜色落到 CSS 初始值 `currentColor` → 暗色主题下描边变成 `#ededf1` 近白。
- **证据**：seen 1（2026-11-04 的路由密钥页 / 调用日志页）。
- **晋升**：`AGENTS.md` §前端样式：Tailwind v4 的裸 `border` 陷阱。
- **应用**：所有前端样式改动。

### L-018

**Tailwind v4 preflight `svg { display: block }`**

- **规则**：按钮里放图标必须显式给 `inline-flex items-center`；`Button` 的 `unstyled` 分支不带任何基础类。
- **why**：Tailwind v4 的 preflight 把 `img, svg, video…` 统一设成 `display: block` → 图标在非 flex 容器里独占一行。`gap-*` 在非 flex 容器里是**空操作**。
- **证据**：seen 1（2026-11-04 的「使用统计」自定义段）。
- **晋升**：`AGENTS.md` §前端样式：Tailwind v4 的 `svg { display: block }` 陷阱。

### L-019

**浮层被祖先 `overflow` 裁掉**

- **规则**：浮层走 `fixed` + `createPortal(document.body)` 逃出滚动容器，位置按 `getBoundingClientRect()` 实测算并夹在视口内。
- **why**：主内容区是 `overflow-auto` 的滚动容器，绝对定位浮层挂在容器内元素上时，越过边界的部分会被裁掉。浮层用 portal 后，「点外面收起」的判定要把 portal 节点算进去。
- **证据**：seen 1（2026-11-04 的选日面板左缘 x=237 vs 容器左界 x=248）。
- **晋升**：`AGENTS.md` §浮层：绝对定位会被祖先的 `overflow` 裁掉。

### L-020

**前端不认识后端合法取值就回落默认值**

- **规则**：后端字段的取值范围比前端 UI 宽时（如 `quotaWindowMs` 是任意毫秒数），前端**不得**把不认识的值回落成某个默认档位再写回 —— 必须**原样保留**；要新增档位就把档位补齐，不认识的走「自定义」原值回写。
- **why**：2026-11-05 实测：后端 `limits.rs` 直接采纳写入的 `quotaWindowMs`（任意毫秒），而前端只认 minute/hour/day 三档，其余回落 day。后果是**用户手改配置写进去的周配额，在 UI 里编辑保存时被静默改写成日配额** —— 典型的「静默失败」，数据损坏且无任何提示。
- **证据**：seen 1（2026-11-05 C-9）。
- **应用**：所有「后端取值域 ⊋ 前端档位」的字段（限额窗口、过期预设、枚举透传）。

### L-021

**字体 preload 的挑选与 crossorigin 剥离是耦合的**

- **规则**：改字体 / 改 preload 时，`frontend/vite.config.ts` 里的 **`pickPreloads()`（构建后从 CSS 的 `@font-face` 块挑）** 与 **`stripCrossOrigin()`（去掉 Vite 加的 `crossorigin`）** 要一起看。
- **why**：字体是 hash 资产（由 CSS `url()` 引用产出），源模板无法按名 preload → 只能在 bundle 写完后解析 CSS；同一批 `<link rel="preload">` 又会被 Vite 加上 `crossorigin` → 而本项目把该属性剥掉（`file://` 场景下会黑屏）。两处都作用于同一批标签。
- **证据**：seen 1（2026-10）。
- **应用**：调整字体集或首屏加载策略时。

---

## E · 语义与架构

### L-022

**修一处要遍历同族**

- **规则**：同一语义在**多条路径**上各有一份实现时（如流式 `forward/stream.rs` 与非流式 `forward/json.rs`），修一处必须**遍历同族**；两条路径的口径差异本身就是缺陷（要显式统一，不许各修各的）。
- **why**：2026-11-04 复核发现「流式只在握手期 4xx 记冷却、非流式对所有状态都记且 2xx 清冷却」—— 同一 provider 在两条路径上的冷却行为不同，任何单侧修复都会留下不一致。
- **证据**：seen 1（2026-11-04 复核 → 2026-11-05 阶段五 5b 统一）。
- **应用**：任何「同一语义在多处实现」的改动。

### L-023

**配置项不是轻量改动**

- **规则**：加一个配置项要牵出**配置 schema + 宽松解析 + 落盘序列化 + 前端契约 + 前端表单 + 兜底默认值**；评估工作量时按这条链算，别按「加个字段」算。
- **why**：2026-11-05 阶段五 5a 原本打算用 provider 配置项做「粘性 cache 价值量化」的开关，最终**放弃**该方案改用协议族判定 —— 因为配置项会把改动面从 3 个文件扩到 6 层，还带来迁移问题，而收益并不更高。
- **证据**：seen 1（2026-11-05 阶段五 5a）。
- **应用**：任何「加个开关」的方案评估。

### L-024

**语义泵重建客户端流时，终止字段整条流只能有一个**

- **规则**：客户端状态机**必须记住**上游给过的结束原因（`finish_reason` / `stop_reason` / `finishReason`），收尾时**只补不含终止字段的尾块**（OpenAI 的 `choices: []` + usage 尾块）+ 哨兵，**绝不再补第二个终止帧**；上游没给结束原因时才兜底补一个默认值。终止字段一旦在一条流里出现两次、且值不同，严格客户端会直接判冲突并拒绝整条流。
- **why**：2026-11-05 —— `ChatClient` 不记上游给过什么，收尾无条件补一个**硬编码** `finish_reason: "stop"` 终块。上游给 `tool_calls` 时，客户端看到 `tool_calls` + `stop` 两个不同值 → Qoder CN 报 `conflicting_finish_reason`；短文本对话因上游也是 `stop`、两值相同而**侥幸正常**（症状随内容而变，最难自查的一类）。同族的 `AnthropicClient` / `ResponsesClient` 都存了 `self.finish` 并在收尾复用 —— **只有 Chat 分支漏了**，这个不对称本身就是缺陷（L-022 的同族遍历）。模块文档里当年写的「黄金基线:上游 finish chunk 与 finalize stop chunk 双块并存」把缺陷写成了规格。
- **证据**：seen 1（2026-11-05，Qoder CN 经 WRouter 调 DeepSeek 工具调用报 `MALFORMED_TOOL_CALL_STREAM: conflicting_finish_reason`；仓库内黄金基线与单测两处复现）。
- **应用**：任何「语义泵重建客户端流」的改动；**新增客户端协议状态机时照抄 Anthropic 分支的 `self.finish` 模式**，不要自己另发明收尾逻辑；改收尾逻辑前先确认同族其它状态机的口径一致。

### L-028

**归一规则必须幂等，否则历史数据与新数据对不上**

- **规则**：任何「把原始值收敛成规范 id」的归一函数都必须满足 **`normalize(normalize(x)) == normalize(x)`**。做不到就别在读侧做第二次归一 —— 幂等是「历史行存原始值、新行存 id」能在**同一个函数**里收敛的前提。
- **why**：2026-11-06 做客户端 UA 归一（`wr_domain::protocol::client_identity`）：写侧存归一 id、读侧再归一次 → 才能让归一落地前的**历史行**（列里是原始 UA）也合并，不用加列、不用迁移。但归一若**不幂等**就会翻车：规范名 `Claude Code` 含空格，而规则第一步是「按空白切首个 token」→ 再归一变成 `Claude` → **两侧对不上**，「按客户端筛选」恒空（且只在选了某个客户端时才复现，不选就一切正常）。解法：别名表同时登记**规范名自身**（`claude code` → `Claude Code`），并在切词**之前**先整串查一次表。
- **证据**：seen 1（2026-11-06，WRouter 客户端维度归一 B-1b）。
- **应用**：任何「写侧存 id / 读侧兜底历史数据」的归一改造；别名/映射表要同时能命中**输入形态**与**输出形态**。

### L-029

**收口帧不是内容帧：done 事件只收口、不重发**

- **规则**：上游协议里带 `done` / `completed` 后缀的事件，默认语义是**收口**（这个 item 到此为止），**不是**「再发一次内容」。解码层遇到这类事件时：① **不得重发开场**（不得再推一次 `ToolCallStart` / `PartStart`）；② 帧里携带的**全文 / 完整参数只作校验**，不进增量通道；③ 已经开过场的 item **只收口一次**。确实需要「上游没给增量、只给了终值」时，走**显式的补齐**通道，且只在「一个字符都没推过」时补一次 —— 绝不重发。
- **why**：2026-11-06 —— Responses 原生上游对同一个 `function_call` 推两次开场（`response.output_item.added` + `response.output_item.done`，后者带**最终**参数），旧解码层两次都当 `ToolCallStart` → 客户端把完整参数收到两遍 → 工具参数被污染成 `{}`（ZCode 实测：8 次子智能体 + 3 次 Bash 全部执行失败）。同族第二处：`response.content_part.done` 携带的是该 part 的**全文**，旧实现当 `PartStart` 重发 → 文本被开两次、说两遍（Qoder 判 `inconsistent_text` 后断连，WRouter 记 499）。第三处：`ResponsesClient` 的 opaque 分支判定写的是 `"output_item.done"`、生产端写的是 `"response.output_item.done"` → **字符串不等，这一支从未命中**（死代码）—— 一旦有人「修好」它，旧逻辑（收口后再把上游 item 推一遍 done）就会让**同一条消息被完成两次**；所以「修死分支」比「留着死分支」更危险。第四处（请求面）：chat 转 Responses 时把 `call_` 开头的来源 id 同时填进 `function_call` 的 `id` 与 `call_id`，违反 Responses 规范（`id` 必须 `fc_` 前缀）→ 严格校验节点 400（`Invalid 'input[7].id': 'call_yDez…'. Expected an ID that begins with 'fc'`）。根因是**同一个**：把「终值帧」当「增量帧」。
- **证据**：seen 1（2026-11-06，ZCode 会话 sess_243861c5 与 Qoder CN 会话 6526b00b 的实测复现）。
- **应用**：任何「多事件协议 → 客户端流」的解码层；新增协议适配器时先确认上游哪些事件是**收口**语义；规范要求 id 前缀的协议（Responses 的 `fc_`）要在**请求面与响应面的全部渲染路径**上统一（L-022 同族遍历：本次共修了 3 条渲染路径）。

### L-030

**分析记录里的「根因编号」不等于实际可触发的路径**

- **规则**：拿到一份「原因 1/2/3/4」的缺陷分析记录后，**先逐条实测坐实可触发路径**，再决定改哪里。记录里的描述可能指向一个**从未被触发的分支**（死代码），而真正在生产里发作的是另一条同族路径。
- **why**：2026-11-06 —— 分析记录把「Qoder 文本重复两遍 + 同一条消息被完成两次」归到 `ResponsesClient` 的 opaque 分支（`close_text_item()` 后透传上游 done item），但实测该分支的判定字符串是 `"output_item.done"`、生产端写的是 `"response.output_item.done"` → **永不命中**。真正让文本重复两遍的是 `content_part.done` 的全文被当 `PartStart` 重发（第二个 `msg_xxx` item 被创建并收口）。若照记录直接去「修好 opaque 分支」，反而会把「同一条消息完成两次」从不可达变成可达 —— 修得越准，越要先证明这条路径真的会被走到。
- **证据**：seen 1（2026-11-06，WRouter 四条 Responses 流式协议缺陷分析）。
- **应用**：任何按外部分析记录排期的缺陷修复；先写最小复现脚本把每条根因跑一遍，再决定落点。

### L-031

**缓存值必须与读取路径的表示一致，有状态引用不进缓存**

- **规则**：响应缓存只能存**读取路径声明的那一种表示**。流式入口不能把上游原始 SSE 字节直接交给非流 JSON 解码器再入缓存；必须先聚合成语义响应，再编码成客户端完整响应 JSON。缓存键还必须在 `previous_response_id` / `conversation` 等有状态引用存在时直接 miss —— 重放无法证明引用归属，宁可不命中。未观察到的 usage 不得写成全零对象。
- **why**：2026-11-07 P5 复核发现：流式缓存把整段 `data: ...` SSE 当 JSON 调 `decode_upstream_json`，解析失败后包成 `{"error":{"message":"<整段 SSE>"}}` 写入缓存；命中时又按客户端 JSON 合成帧，语义完全错误。另有一个同源问题：`semantic_to_domain` 只取 `usage.total`，Anthropic 不报 total 时流式终帧把用量当 0 而省略，但非流 JSON 却按 input+output 算出非零值，违反 C14 跨表示一致。修复即：缓存存聚合后的客户端 JSON、stateful 请求禁用缓存、total 缺省按 input+output 计算。
- **证据**：seen 1（2026-11-07 P5 缓存/A6 接线；旧路径 `forward/stream.rs` 的 `upstream_raw` + `decode_upstream_json` 组合）。
- **应用**：任何「写缓存/重放」路径；任何在多种表示之间共享缓存条目的设计；任何 usage 全 Option 的协议适配。

### L-032

**Windows 上 `child.kill()` 杀不掉 npx 底下的真实进程**

- **规则**：Windows + `shell: true` 启动的子进程，`child.kill()` 只结束 `cmd.exe` 包装层；`npx -> node -> vite` 这条链上的真实进程会变成孤儿。清理必须用 `taskkill /PID <pid> /T /F` 杀整棵树，并放在同步的 `process.once("exit")` 钩子里；不要依赖异步 `setTimeout` 清理，父进程会先退出。
- **why**：2026-11-08 排查 `npm run dev` 起不来：5281/5282 上残留着前几次 dev 的 vite，父进程是仍活着的 `cmd.exe`/`npx`，正是旧代码在 Tauri 退出时 `fe.kill()` 只杀包装层留下的。随后单靠扩大端口候选只会让每次中断多留一个孤儿，最终耗尽 5280–5285。
- **证据**：seen 1（2026-11-08，`scripts/dev.mjs` 退出清理；实测 taskkill /T 后 5280 无监听、无残留 node）。
- **应用**：任何 Windows 上通过 `shell:true`/`npx`/`cargo run` 启动的开发编排脚本；退出路径必须覆盖正常退出、SIGINT/SIGTERM/SIGHUP/SIGBREAK 与异常退出。

### L-033

**集成测试共用固定临时目录，并行时撞 SQLite 锁**

- **规则**：集成测试的 `data_dir` / `config_dir` 必须唯一化 —— 用「进程 id + 递增序号」或 `tag + pid`，不要写死 `temp_dir().join("xxx-test-data")`。
- **why**：2026-11-14 —— `tests/health.rs` 的 `test_settings()` 让三个测试共用 `wr-server-test-data`，CI 上并行执行同时打开同一个 SQLite 文件，在 `PRAGMA journal_mode=WAL` 阶段争抢锁，报 `Storage("pragma failed: database is locked")`，`unknown_route_is_404` 与 `management_health_matches_python_shape` 偶发失败（`-p wr-server --test health`）。本地 CPU 并发低，前两次 CI 都通过，第三次才暴露，一度被误判成网络抖动去重跑。
- **证据**：seen 1（2026-11-14，run 43609690913 日志；修复见 `fix(test): health 测试改用独立临时目录`）。
- **应用**：任何新建的集成测试、任何在测试里 `SqliteStore::open` 或写配置文件的地方。`gateway/common.rs` 与 `routing.rs` 是靠调用点覆盖 `settings.data_dir` 才没踩，新代码不要依赖这种隐式约定。
