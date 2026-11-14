## 动机

<!-- 解决什么问题；关联 issue：Fixes # -->

## 改动

<!-- 逐条列出，注明关键文件 -->

## 类型

- [ ] 小修复（文档 / 依赖 / 兼容性）
- [ ] 其他（请先在 issue 中对齐，未对齐的 PR 可能被关闭）

## 验证

<!-- 跑过哪些命令、覆盖了哪些场景 -->

```text
bash scripts/check-rust.sh
cd frontend && npm run typecheck && npm test
node scripts/validate-capabilities.mjs
```

## 自检

- [ ] 每个提交带 `Signed-off-by`（`git commit -s`）
- [ ] 基于最新 `master`，CI 通过
- [ ] 未提交构建产物、密钥或本地配置
- [ ] 未混入格式化 / 无关重构
