#!/usr/bin/env bash
# WRouter Linux 安装包构建编排 (20261102_01 P2-4: 纯后端交付, 去掉 Electron).
#
# 宿主: build-backend.sh → build-rust.sh --target musl  → 静态单二进制 (musl)
#       node scripts/generate-build-info.mjs            → 构建清单
#       npm --prefix frontend run build                → 前端 SPA dist
# 容器: rpmbuild -bb packaging/wrouter.spec            → RPM (纯后端 + SPA + systemd 单元)
#
# 与旧链路的关键差异 (P2):
#   - 不再拉 Electron zip、不再挂 electron cache 卷、不再跑 electron-builder;
#   - 不再产 AppImage (决策 D1);
#   - 容器内只做 rpmbuild —— 不经 fpm 的 portable ruby, 历史 "rpmbuild EVP_md2"
#     符号错误不复存在;
#   - musl 静态二进制 → RPM 无 glibc 依赖, 老系统(麒麟 V10 / UOS / CentOS 7)可装;
#   - 构建在容器内 ext4 (/tmp) 进行 —— 规避 NTFS 挂载卷 rename 坑。
#
# 构建镜像 (硬约束): 沿用 wrouter-linux-builder:v2 (跨应用共享, 禁止删除/裁剪/重建)。
#
# 用法:
#   packaging/build-linux.sh                    # 构建 RPM
#   packaging/build-linux.sh --dry-run          # 只测试环境, 不构建
#
# 环境变量:
#   WROUTER_BUILDER_IMAGE — 构建镜像覆盖 (默认 wrouter-linux-builder:v2)
#   HTTP_PROXY             — 自定义代理 (本链路已无外网下载, 仅透传给容器备用)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# 转换为 Windows 路径 (Git Bash → Docker Desktop)
if command -v cygpath &>/dev/null; then
  SCRIPT_DIR="$(cygpath -w "$SCRIPT_DIR" 2>/dev/null | sed 's/\\/\//g' || echo "$SCRIPT_DIR")"
  PROJECT_ROOT="$(cygpath -w "$PROJECT_ROOT" 2>/dev/null | sed 's/\\/\//g' || echo "$PROJECT_ROOT")"
fi
# 构建镜像: 默认沿用共享的 wrouter-linux-builder:v2 (含 rpmbuild 4.14.3);
# 可用 WROUTER_BUILDER_IMAGE 指向其它镜像。
IMAGE="${WROUTER_BUILDER_IMAGE:-wrouter-linux-builder:v2}"
DRY_RUN=false

# Allow the caller (scripts/build.mjs) to place artifacts under
# build/dist/<stamp>/linux/ instead of the default dist/.
# build.mjs passes WROUTER_BUILD_STAMP (a plain timestamp string, no path) to
# avoid Windows/Unix path conversion issues with Docker volume mounts.
if [ -n "${WROUTER_BUILD_STAMP:-}" ]; then
  DIST_DIR="$PROJECT_ROOT/build/dist/$WROUTER_BUILD_STAMP/linux"
else
  DIST_DIR="$PROJECT_ROOT/dist"
fi

# 解析参数
for arg in "$@"; do
  case "$arg" in
    --dry-run)
      DRY_RUN=true
      ;;
    --proxy)
      echo "[build-linux] NOTE: P2 起本链路无外网下载 (无 Electron / AppImage), --proxy 仅透传备用"
      ;;
    --help)
      echo "Usage: $0 [--proxy] [--dry-run]"
      echo "  --proxy   透传代理 (本链路已无外网下载需求)"
      echo "  --dry-run 只测试容器环境, 不构建"
      exit 0
      ;;
  esac
done

echo "[build-linux] Project root: $PROJECT_ROOT"
echo "[build-linux] Image: $IMAGE"
echo "[build-linux] Dist dir: $DIST_DIR"

# ---------------------------------------------------------------------------
# 目标校验: P2 起只产 RPM (决策 D1: 不产 AppImage)
# ---------------------------------------------------------------------------
if [ -n "${WROUTER_LINUX_TARGETS:-}" ] && [ "$WROUTER_LINUX_TARGETS" != "rpm" ]; then
  echo "[build-linux] ERROR: 目标 '$WROUTER_LINUX_TARGETS' 已下线 —— P2 起 Linux 只产 RPM (决策 D1)"
  exit 1
fi

# Dry-run: 只测试环境
if [ "$DRY_RUN" = true ]; then
  echo "[build-linux] Dry-run mode: checking container environment..."
  docker run --rm "$IMAGE" \
    bash -lc '
      echo "=== Container Environment ==="
      echo "rpmbuild: $(rpmbuild --version 2>&1 | head -1)"
      echo "rpm: $(rpm --version 2>&1 | head -1)"
      echo "Node: $(node --version)"
      echo ""
      echo "=== Container Filesystem ==="
      echo "tmp fs: $(df -T /tmp | tail -1 | awk "{print \$2}")"
    '
  exit 0
fi

# ---------------------------------------------------------------------------
# 宿主: 构建清单 (须在 build-backend.sh 之前 —— 它会把清单拷进暂存目录)
# ---------------------------------------------------------------------------
echo "[build-linux] Generating build info..."
node "$PROJECT_ROOT/scripts/generate-build-info.mjs"

# ---------------------------------------------------------------------------
# 宿主: 构建 musl 静态后端并暂存
# ---------------------------------------------------------------------------
echo "[build-linux] Building Rust backend on host (musl static)..."
bash "$SCRIPT_DIR/build-backend.sh"

# ---------------------------------------------------------------------------
# 宿主: 构建前端 (vite → frontend/dist)。容器内无 node 构建链。
# ---------------------------------------------------------------------------
echo "[build-linux] Building frontend on host (vite)..."
npm --prefix "$PROJECT_ROOT/frontend" run build

# 版本号: 唯一来源 = 根 package.json (与后端 WR_VERSION 注入同源)
VERSION="$(cd "$PROJECT_ROOT" && node -p "require('./package.json').version")"

# 确保 dist 目录存在
mkdir -p "$DIST_DIR"

# ---------------------------------------------------------------------------
# 容器: 装配 stage → rpmbuild → 拷回产物
# ---------------------------------------------------------------------------
echo "[build-linux] Starting container build (rpmbuild)..."
# MSYS_NO_PATHCONV prevents Git Bash from mangling container-internal paths
# like /build/dist into Windows paths, which would make Docker create stray
# host directories (e.g. an empty top-level dist/).
MSYS_NO_PATHCONV=1 docker run --rm \
  -v "$PROJECT_ROOT:/build" \
  -v "$DIST_DIR:/build/dist" \
  -e "WROUTER_RPM_VERSION=$VERSION" \
  -u root \
  "$IMAGE" \
  bash -lc '
    set -euo pipefail
    export PATH="/usr/bin:/usr/sbin:/usr/local/bin:$PATH"

    echo ""
    echo "=============================================="
    echo " WRouter Linux Build (rpmbuild, pure backend)"
    echo " rpmbuild: $(rpmbuild --version 2>&1 | head -1)"
    echo " version: ${WROUTER_RPM_VERSION}"
    echo "=============================================="
    echo ""

    # ---- Step 1: 装配 stage (容器内 ext4) ----
    echo "[container] Step 1: Assembling stage on ext4 (/tmp/stage)..."
    rm -rf /tmp/stage /tmp/rpmbuild
    mkdir -p /tmp/stage/frontend

    # 单二进制 (musl 静态)
    if [ ! -f /build/backend/dist/electron/wr-backend ]; then
      echo "[container] ERROR: /build/backend/dist/electron/wr-backend missing (host build failed?)"
      exit 1
    fi
    cp /build/backend/dist/electron/wr-backend /tmp/stage/wr-backend
    chmod 0755 /tmp/stage/wr-backend

    # 构建清单 (宿主 build-backend.sh 已拷进暂存目录)
    if [ -f /build/backend/dist/electron/build-info.json ]; then
      cp /build/backend/dist/electron/build-info.json /tmp/stage/build-info.json
    else
      echo "[container] WARN: build-info.json missing, generating a minimal one"
      printf "{\n  \"version\": \"%s\",\n  \"buildTool\": \"rpmbuild\"\n}\n" "${WROUTER_RPM_VERSION}" \
        > /tmp/stage/build-info.json
    fi

    # 前端 SPA
    if [ ! -f /build/frontend/dist/index.html ]; then
      echo "[container] ERROR: /build/frontend/dist/index.html missing (host vite build failed?)"
      exit 1
    fi
    cp -R /build/frontend/dist/. /tmp/stage/frontend/
    chmod -R a+rX /tmp/stage/frontend

    # systemd 单元 / 能力契约 / 安装钩子
    cp /build/packaging/wrouter.service /tmp/stage/wrouter.service
    cp /build/contracts/wrouter-capabilities.json /tmp/stage/wrouter-capabilities.json
    cp /build/packaging/after-install.sh /tmp/stage/after-install.sh
    cp /build/packaging/after-remove.sh /tmp/stage/after-remove.sh
    chmod 0644 /tmp/stage/wrouter.service /tmp/stage/wrouter-capabilities.json
    chmod 0755 /tmp/stage/after-install.sh /tmp/stage/after-remove.sh

    # ---- Step 2: rpmbuild ----
    echo "[container] Step 2: rpmbuild -bb..."
    mkdir -p /tmp/rpmbuild/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
    rpmbuild -bb \
      --define "_topdir /tmp/rpmbuild" \
      --define "_wr_version ${WROUTER_RPM_VERSION}" \
      --define "_wr_stage /tmp/stage" \
      /build/packaging/wrouter.spec

    echo ""
    echo "=============================================="
    echo " Build complete! Copying artifacts to host..."
    cp /tmp/rpmbuild/RPMS/x86_64/*.rpm /build/dist/ 2>/dev/null || {
      echo "[container] ERROR: no .rpm produced"
      exit 1
    }
    ls -la /build/dist/
    echo "=============================================="
  '

echo "[build-linux] Done! Artifacts in $DIST_DIR:"
ls -la "$DIST_DIR/"
