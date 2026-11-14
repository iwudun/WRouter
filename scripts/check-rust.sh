#!/usr/bin/env bash
# 方案 #14 R1.10:Rust 侧质量门(仓库已无独立的 check.ps1)。
#   1. wr-domain 纯度守护(R1.2):禁 async 运行时/Web/HTTP/SQLite 依赖
#   2. cargo fmt --check
#   3. cargo clippy -D warnings
#   4. cargo test
# 注:fmt/clippy 在 master 上存在既有红,按 AGENTS.md 做增量判定,不整体修。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKEND_RS="$(cd "$SCRIPT_DIR/../backend" && pwd)"

# windows-gnu 宿主需要 rustup 自带 MinGW binutils 在 PATH(见 build-rust.sh)。
if [[ "$(rustc -vV | grep host | awk '{print $2}')" == "x86_64-pc-windows-gnu" ]]; then
    SELF_CONTAINED="$(rustc --print sysroot)/lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained"
    [[ -d "$SELF_CONTAINED" ]] && export PATH="$SELF_CONTAINED:$PATH"
fi

cd "$BACKEND_RS"

# ---- 纯度守护:wr-domain 的 Cargo.toml 不得出现禁用依赖 ----
if grep -Eq '^\s*(tokio|axum|reqwest|rusqlite|hyper|tower)\b' crates/wr-domain/Cargo.toml; then
    echo "[check-rust] 纯度违规:wr-domain 不得依赖 async 运行时/Web/HTTP/SQLite crate(方案 #14 R1.2)" >&2
    exit 1
fi
echo "[check-rust] wr-domain 纯度守护 OK"

echo "[check-rust] cargo fmt --check"
cargo fmt --all -- --check

echo "[check-rust] cargo clippy -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "[check-rust] cargo test"
cargo test --workspace

echo "[check-rust] 全部通过"
