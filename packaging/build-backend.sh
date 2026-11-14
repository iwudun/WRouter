#!/usr/bin/env bash
# WRouter 后端 Linux 构建脚本 (Rust 单二进制, 方案 #14 R6.2).
#
# 在宿主机执行(需要 rustup + cargo-zigbuild + zig, 见 backend/README.md):
# 构建 musl 静态轨并暂存到 backend/dist/electron/wr-backend(目录名为历史
# 约定,当前只服务 Linux RPM 打包),供 build-linux.sh 在容器内取用。
# 容器内无 rust 工具链 —— musl 静态二进制与目标机 glibc 无关,宿主交叉构建即可。
#
# 用法:
#   packaging/build-backend.sh              # 缺归档或源码较新时构建,否则直接暂存
#   packaging/build-backend.sh --force      # 无条件重新构建

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BACKEND_RS="$PROJECT_ROOT/backend"
STAGING="$BACKEND_RS/dist/electron"
FORCE=false
if [[ "${1:-}" == "--force" ]]; then FORCE=true; fi

# 版本注入(R1.5 约定:唯一来源 = 根 package.json)
VERSION="$(cd "$PROJECT_ROOT" && node -p "require('./package.json').version")"
TARBALL="$BACKEND_RS/dist/wr-backend-$VERSION-x86_64-unknown-linux-musl.tar.gz"

# 新鲜度守卫:tarball 存在但 backend/ 下源码(target/dist 除外)比它新时
# 自动重编,防止同版本号改代码后静默打包旧后端。伪触发(git 操作等刷了
# mtime 而内容未变)由 cargo 内容指纹兜底,只付一次空跑检查。
STALE_REASON=""
if $FORCE; then
  STALE_REASON="--force"
elif [[ ! -f "$TARBALL" ]]; then
  STALE_REASON="no tarball for v$VERSION"
else
  NEWER_FILE="$(find "$BACKEND_RS" -type f -not -path "*/target/*" -not -path "*/dist/*" -newer "$TARBALL" -print -quit)"
  if [[ -n "$NEWER_FILE" ]]; then
    STALE_REASON="source newer than tarball: ${NEWER_FILE#"$BACKEND_RS"/}"
  fi
fi

if [[ -n "$STALE_REASON" ]]; then
  echo "[build-backend] Building musl static binary (build-rust.sh): $STALE_REASON"
  bash "$SCRIPT_DIR/build-rust.sh" --target musl
else
  echo "[build-backend] Reusing existing tarball (backend sources unchanged): $TARBALL"
fi

echo "[build-backend] Staging extraResources dir: $STAGING"
rm -rf "$STAGING"
mkdir -p "$STAGING"
tar -xzf "$TARBALL" -C "$STAGING"
mv "$STAGING/wr" "$STAGING/wr-backend"
chmod +x "$STAGING/wr-backend"

# 构建清单随包(沿用旧产物面;缺失时跳过)
if [[ -f "$PROJECT_ROOT/build/build-info.json" ]]; then
  cp -f "$PROJECT_ROOT/build/build-info.json" "$STAGING/build-info.json"
fi

echo "[build-backend] Build complete!"
echo "  Staged: $STAGING/wr-backend"
