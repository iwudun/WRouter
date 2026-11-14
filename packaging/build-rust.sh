#!/usr/bin/env bash
# 方案 #14 R1.7/R1.8:Rust 后端构建脚本(Linux 交付双轨 + 宿主平台)。
#
# 用法:
#   packaging/build-rust.sh                 # 默认:musl 静态主交付轨
#   packaging/build-rust.sh --target gnu228 # GNU 2.28 备选轨(cargo-zigbuild)
#   packaging/build-rust.sh --target host   # 当前宿主(debug 用,不归档)
#
# 依赖(见 backend/README.md):
#   - rustup(rust-toolchain.toml 锁定 rustc 版本)
#   - cargo-zigbuild + zig(Linux 双轨交叉编译)
#   - 版本号来源:根 package.json 的 version → 构建时注入 WR_VERSION
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BACKEND_RS="$REPO_ROOT/backend"
TARGET_KIND="musl"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            TARGET_KIND="$2"
            shift 2
            ;;
        *)
            echo "未知参数: $1(支持 --target musl|gnu228|host)" >&2
            exit 1
            ;;
    esac
done

# ---- 版本注入(R1.5):唯一来源 = 根 package.json ----
# 注:cwd 相对读取,避免 Git Bash 的 MSYS 路径(/e/...)无法被 Windows
# node 的 require 解析。
VERSION="$(cd "$REPO_ROOT" && node -p "require('./package.json').version")"
export WR_VERSION="$VERSION"
echo "[build-rust] WR_VERSION=$VERSION"

cd "$BACKEND_RS"

# windows-gnu 宿主:把 rustup 自带的 MinGW binutils(dlltool 等)加进
# PATH,否则 windows-sys 的 build script 找不到 dlltool。rustup 自带
# dlltool 缺汇编器(CreateProcess 失败),因此还需要 WinLibs MinGW:
# 优先 WROUTER_WINLIBS_BIN,否则扫 winget 安装目录。
if [[ "$(rustc -vV | grep host | awk '{print $2}')" == "x86_64-pc-windows-gnu" ]]; then
    SELF_CONTAINED="$(rustc --print sysroot)/lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained"
    [[ -d "$SELF_CONTAINED" ]] && export PATH="$SELF_CONTAINED:$PATH"
    if ! command -v dlltool &>/dev/null; then
        for cand in "${WROUTER_WINLIBS_BIN:-}" \
                    "$HOME/AppData/Local/Microsoft/WinGet/Packages/"BrechtSanders.WinLibs*/mingw64/bin; do
            [[ -n "$cand" && -d "$cand" ]] && export PATH="$cand:$PATH" && break
        done
    fi
    if ! command -v dlltool &>/dev/null; then
        echo "[build-rust] ERROR: dlltool not on PATH (windows-gnu 宿主需要 WinLibs MinGW, 见 backend/README.md)" >&2
        exit 1
    fi
fi

# ---- 目标轨(方案 #14 D5)----
# musl:静态主交付;gnu228:x86_64-unknown-linux-gnu.2.28(cargo-zigbuild
# 直接支持 .2.28 后缀,产出 glibc 2.28 兼容的动态轨,兜底 musl DNS 差异)。
DIST="$BACKEND_RS/dist"
mkdir -p "$DIST"

case "$TARGET_KIND" in
    musl)
        TARGET="x86_64-unknown-linux-musl"
        rustup target add "$TARGET"
        cargo zigbuild --release --target "$TARGET"
        BINARY="target/$TARGET/release/wr"
        ARTIFACT="$DIST/wr-backend-$VERSION-$TARGET.tar.gz"
        tar -czf "$ARTIFACT" -C "$(dirname "$BINARY")" "$(basename "$BINARY")"
        ;;
    gnu228)
        TARGET="x86_64-unknown-linux-gnu.2.28"
        rustup target add x86_64-unknown-linux-gnu
        cargo zigbuild --release --target "$TARGET"
        BINARY="target/x86_64-unknown-linux-gnu/release/wr"
        ARTIFACT="$DIST/wr-backend-$VERSION-$TARGET.tar.gz"
        tar -czf "$ARTIFACT" -C "$(dirname "$BINARY")" "$(basename "$BINARY")"
        ;;
    host)
        cargo build --release
        echo "[build-rust] host 构建:$BACKEND_RS/target/release/wr(不归档)"
        exit 0
        ;;
    *)
        echo "未知 target: $TARGET_KIND" >&2
        exit 1
        ;;
esac

echo "[build-rust] 产物:$ARTIFACT"
shasum -a 256 "$ARTIFACT" > "$ARTIFACT.sha256" 2>/dev/null || sha256sum "$ARTIFACT" > "$ARTIFACT.sha256"
echo "[build-rust] 校验和:$(cat "$ARTIFACT.sha256")"
