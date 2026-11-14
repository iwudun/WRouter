# 参与贡献

感谢你关注 WRouter。提交前请先读本文件，能省掉大部分来回。

## 先看这里

- **大改动先开 issue**，说明场景、动机与大致方案，对齐后再写代码。没讨论过的大功能 PR 通常不会被合并。
- **只接受小修复**：文档、错别字、依赖升级、明确的兼容性问题。其余改动请先开 issue。
- 本项目主开发在 Gitee，GitHub 上的代码是按需同步过去的快照；两边**已同步的提交 SHA 一致**。

## 开发环境

| 依赖 | 版本 |
| --- | --- |
| Node.js | ≥ 20（推荐 22） |
| Rust | 1.98.0（`backend/rust-toolchain.toml` 锁定） |
| Python | 3.8+（仅开发辅助脚本） |
| Docker Desktop | 仅 Linux 打包需要 |

```bash
npm run setup     # 安装依赖并构建后端二进制
npm run dev       # 启动 Vite + Tauri 桌面壳
```

细节见 `docs/reference/05-开发指南.md` 与 `docs/reference/06-构建与发布.md`。

## 提交规范

1. 每个提交做一件事，message 用 `type(scope): summary`，例如 `fix(router): 修正重试上限`。
2. **必须签署 DCO**：`git commit -s`，提交信息末尾会出现

   ```text
   Signed-off-by: Your Name <you@example.com>
   ```

   姓名与邮箱须与提交作者一致；网页端提交请勾选 “Sign off”。
3. 不要提交构建产物、`.dev/`、`node_modules/`、`target/`、密钥或本地配置。

## 提交前自检

```bash
bash scripts/check-rust.sh                 # 后端质量门
cd frontend && npm run typecheck && npm test
node scripts/validate-capabilities.mjs     # RPC 契约一致性
npm run version:check                      # 版本号一致
```

`cargo fmt --check`、`cargo clippy -D warnings`、`npm run typecheck:unused` 在主干上本来就有红，按“本次是否新增”判定，不要顺手全仓格式化。

## PR 规则

- PR 基于**已同步的 `master`**；落后就先 rebase。
- 一个 PR 只做一件事，不混格式化、重构与功能。
- CI 必须通过；至少一名维护者 review 后由维护者 **squash merge**。
- 合并后维护者会把结果回合到 Gitee；你不需要做额外操作。
- 方向不符、改动过大、引入新依赖或改 CI 脚本的 PR，维护者可以直接关闭并说明原因。

## 报告问题

- 功能缺陷用 issue 模板，附版本、平台、复现步骤与日志。
- 安全漏洞**不要**开公开 issue，见 `SECURITY.md`。

## 许可

提交即表示你同意以 [MIT](LICENSE) 许可发布你的贡献，并确认你有权提交这些代码（DCO 的作用即在此）。本项目不要求签署 CLA。
