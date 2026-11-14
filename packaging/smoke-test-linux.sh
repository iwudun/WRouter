#!/usr/bin/env bash
# WRouter Linux RPM 冒烟测试 (20261102_01 P2-7: 纯后端交付).
#
# 在 Rocky Linux 8 容器内执行, 验证 RPM 的安装 / 启动 / 卸载。
# 零本机影响 —— 所有操作在 Docker 隔离环境中。
#
# 交付形态 (P2): 单 musl 静态二进制 (wr-backend) + 前端 SPA + systemd 用户单元。
# 无 Electron / WebKitGTK / GPU / 沙箱 / FUSE。
#
# 用法:
#   packaging/smoke-test-linux.sh                    # 检查 dist/ 中的最新 RPM
#   packaging/smoke-test-linux.sh /path/to/dist      # 指定产物目录

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DIST_DIR="${1:-$PROJECT_ROOT/dist}"
IMAGE="${2:-rockylinux:8}"

echo "[smoke-test-linux] Dist dir: $DIST_DIR"
echo "[smoke-test-linux] Image: $IMAGE"

# 查找最新 RPM (rpmbuild 命名: wrouter-<version>-<release>.<arch>.rpm)
RPM_FILE=$(ls -t "$DIST_DIR"/*.rpm 2>/dev/null | head -1 || true)

echo "[smoke-test-linux] RPM: ${RPM_FILE:-(none found)}"

if [ -z "$RPM_FILE" ]; then
  echo "[smoke-test-linux] FAIL: No RPM found in $DIST_DIR"
  exit 1
fi

RPM_NAME=$(basename "$RPM_FILE")
RPM_SIZE=$(stat -c%s "$RPM_FILE" 2>/dev/null || stat -f%z "$RPM_FILE" 2>/dev/null)
echo "  Size: $((RPM_SIZE / 1024 / 1024)) MB"

# Git Bash → Windows 路径
if command -v cygpath &>/dev/null; then
  RPM_FILE="$(cygpath -w "$RPM_FILE" | sed 's/\\/\//g')"
fi

echo ""
echo "=== RPM Smoke Test: $RPM_NAME ==="

# 在容器内测试
docker run --rm -v "$RPM_FILE:/tmp/wrouter.rpm:ro" "$IMAGE" bash -c '
  set -euo pipefail
  FAIL=0
  step() { echo ""; echo "=== $1 ==="; }
  ok()   { echo "  PASS: $1"; }
  bad()  { echo "  FAIL: $1"; FAIL=1; }

  step "1. Installing RPM..."
  # 纯后端包无 GUI 依赖 → 直接 rpm 安装即可 (无需 dnf 解析依赖)。
  rpm -ivh --nodeps /tmp/wrouter.rpm 2>&1 | tail -3
  rpm -qa | grep -i "^wrouter" >/dev/null 2>&1 && ok "package installed" || bad "package not installed"

  step "2. Verifying RPM contents (no Electron / webkit / mesa)"
  rpm -qpl /tmp/wrouter.rpm | head -20
  echo "  --- forbidden payload scan ---"
  if rpm -qpl /tmp/wrouter.rpm | grep -Ei "electron|webkit|mesa|\.so\.|gtk|libX11|chromium"; then
    bad "RPM contains Electron/webkit/mesa/shared-library payload"
  else
    ok "no Electron / webkit / mesa / shared-library payload"
  fi

  step "3. Checking single binary (musl static) + SPA + unit"
  [ -x /usr/bin/wr-backend ] && ok "/usr/bin/wr-backend installed (executable)" \
    || bad "/usr/bin/wr-backend missing or not executable"
  # 静态性判定:rockylinux:8 精简镜像无 file 命令 → 用 ldd(静态二进制报
  # "not a dynamic executable")。
  # 注意:ldd 对静态二进制 exit 1 → 不能直接进管道(pipefail 会判失败),
  # 先捕获输出再匹配。
  LDD_OUT=$(ldd /usr/bin/wr-backend 2>&1 || true)
  if echo "$LDD_OUT" | grep -q "not a dynamic executable"; then
    ok "binary is statically linked (no glibc dependency)"
  elif command -v file &>/dev/null && file /usr/bin/wr-backend | grep -q "statically linked"; then
    ok "binary is statically linked (no glibc dependency)"
  else
    bad "binary is NOT statically linked: ${LDD_OUT}"
  fi
  [ -f /usr/share/wrouter/frontend/index.html ] \
    && ok "frontend SPA present (/usr/share/wrouter/frontend)" \
    || bad "frontend SPA missing"
  [ -f /usr/share/wrouter/contracts/wrouter-capabilities.json ] \
    && ok "capabilities contract present" \
    || bad "capabilities contract missing"
  SERVICE_FILE=/usr/lib/systemd/user/wrouter.service
  [ -f "$SERVICE_FILE" ] && ok "systemd user unit installed: $SERVICE_FILE" \
    || bad "$SERVICE_FILE not installed"
  grep -q "/usr/bin/wr-backend serve" "$SERVICE_FILE" \
    && ok "ExecStart points at /usr/bin/wr-backend" \
    || bad "ExecStart does not point at /usr/bin/wr-backend: $(grep ExecStart "$SERVICE_FILE" || true)"
  grep -q "WROUTER_FRONTEND_DIR=/usr/share/wrouter/frontend" "$SERVICE_FILE" \
    && ok "unit sets WROUTER_FRONTEND_DIR" \
    || bad "unit missing WROUTER_FRONTEND_DIR"

  step "4. Backend binary runs (--version)"
  /usr/bin/wr-backend --version

  step "5. Serving SPA + management API (browser reachability)"
  DATA_DIR=/tmp/wrouter-smoke-data
  rm -rf "$DATA_DIR"
  WROUTER_WEB_PORT=34622 WROUTER_GATEWAY_PORT=34611 WROUTER_SERVE_UI=1 \
  WROUTER_FRONTEND_DIR=/usr/share/wrouter/frontend \
  WROUTER_CAPABILITIES_FILE=/usr/share/wrouter/contracts/wrouter-capabilities.json \
    /usr/bin/wr-backend serve --host 127.0.0.1 --data-dir "$DATA_DIR" >/tmp/wr.log 2>&1 &
  WR_PID=$!
  # 等管理面就绪
  READY=0
  for _ in $(seq 1 40); do
    if curl -fsS http://127.0.0.1:34622/api/v1/health >/dev/null 2>&1; then READY=1; break; fi
    sleep 0.5
  done
  [ "$READY" = "1" ] && ok "management health ok (127.0.0.1:34622)" || { bad "management health check failed"; tail -20 /tmp/wr.log; }

  if [ "$READY" = "1" ]; then
    # SPA 入口页
    curl -fsS http://127.0.0.1:34622/ | grep -qi "<div id=\"root\">" \
      && ok "GET / returns SPA index.html" \
      || bad "GET / did not return SPA index.html"
    # 静态资源 (vite 产物在 assets/ 下, 相对路径 base=./)
    ASSET=$(cd /usr/share/wrouter/frontend/assets && ls *.js 2>/dev/null | head -1 || true)
    if [ -n "$ASSET" ]; then
      curl -fsS -o /dev/null "http://127.0.0.1:34622/assets/$ASSET" \
        && ok "GET /assets/$ASSET ok" \
        || bad "GET /assets/$ASSET failed"
    fi
    # SPA fallback (深链回落到 index.html)
    curl -fsS http://127.0.0.1:34622/settings/network | grep -qi "<div id=\"root\">" \
      && ok "SPA fallback works on deep link" \
      || bad "SPA fallback broken"
    # API 404 不被 SPA fallback 吞掉(注意:404 时 curl -f 会丢弃 body,
    # 故此处用 -s 不带 -f,再校验 JSON 形态)
    API404=$(curl -s http://127.0.0.1:34622/api/v1/nonexistent 2>/dev/null)
    case "$API404" in
      *'"detail"'*"Not Found"*) ok "API 404 not swallowed by SPA fallback" ;;
      *) bad "API 404 was swallowed (got: ${API404:0:120})" ;;
    esac
    # session 端点 (loopback-only)
    curl -fsS http://127.0.0.1:34622/api/wr/session | grep -q "token" \
      && ok "GET /api/wr/session returns token" \
      || bad "/api/wr/session did not return token"
  fi

  step "6. Removing RPM"
  PKG_NAME=$(rpm -qf /usr/bin/wr-backend --qf "%{name}\n" 2>/dev/null || echo wrouter)
  rpm -e "$PKG_NAME" 2>&1 | tail -3
  if [ -f "$SERVICE_FILE" ]; then
    bad "$SERVICE_FILE still present after rpm -e"
  else
    ok "systemd unit removed with the package"
  fi
  [ -x /usr/bin/wr-backend ] && bad "/usr/bin/wr-backend still present after rpm -e" \
    || ok "/usr/bin/wr-backend removed"
  [ -f /usr/share/wrouter/frontend/index.html ] && bad "SPA still present after rpm -e" \
    || ok "frontend SPA removed with the package"

  step "7. Verifying user data retained"
  [ -d "$DATA_DIR" ] && ok "user data dir retained ($DATA_DIR)" || bad "user data dir missing"

  # 停掉后台后端
  kill "$WR_PID" 2>/dev/null || true

  echo ""
  if [ "$FAIL" = "0" ]; then
    echo "RPM SMOKE PASS"
  else
    echo "RPM SMOKE FAIL"
    exit 1
  fi
' 2>&1 | sed "s/^/  /"

echo ""
echo "[smoke-test-linux] PASS"
