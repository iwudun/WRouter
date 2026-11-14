#!/usr/bin/env bash
# R6.4 / 20261102_01 P2-7: wrouter.service systemd 冒烟 (Rocky 8 容器, systemd as PID1)。
#
# 验证: 单元安装 → 用户服务启停 → 开机自启(is-enabled) →
#       进程被杀后 Restart=on-failure 自动拉起 → journalctl --user 有日志 →
#       管理面返回 SPA 且 /api/wr/session 可达 (浏览器可达性)。
# 前置: RPM 产物 (P2 起为纯后端包: 单 musl 静态二进制 + SPA + systemd 用户单元)。
#
# 用法:
#   packaging/smoke-test-systemd.sh /path/to/wrouter-*.rpm [image]
#
# 说明: 容器无法真实重启, "开机自启" 以 is-enabled=WantedBy default.target
#       校验代替; 其余(启停/自动重启/journald)均为真实 systemd 行为。

set -euo pipefail

RPM_FILE="${1:?usage: smoke-test-systemd.sh <wrouter-*.rpm> [image]}"
IMAGE="${2:-rockylinux:8}"
CID="wr-systemd-smoke-$$"

cleanup() { docker rm -f "$CID" &>/dev/null || true; }
trap cleanup EXIT

# 相对路径 → 绝对路径(docker -v 只接受绝对路径)
case "$RPM_FILE" in
  /*) ;;
  *) RPM_FILE="$(pwd)/$RPM_FILE" ;;
esac

# Git Bash → Windows 路径
if command -v cygpath &>/dev/null; then
  RPM_FILE="$(cygpath -w "$RPM_FILE" | sed 's/\\/\//g')"
fi

echo "[smoke-systemd] RPM: $(basename "$RPM_FILE" 2>/dev/null || echo "$RPM_FILE")"
echo "[smoke-systemd] Image: $IMAGE"

echo "[smoke-systemd] Starting systemd container (privileged, PID1=/usr/sbin/init)..."
MSYS_NO_PATHCONV=1 docker run -d --privileged --name "$CID" \
  -v "$RPM_FILE:/tmp/wrouter.rpm:ro" \
  "$IMAGE" /usr/sbin/init >/dev/null

# 等 systemd 就绪 (容器内 systemctl is-system-running 可能返回 degraded, 能响应即可)
for _ in $(seq 1 30); do
  if docker exec "$CID" systemctl is-system-running &>/dev/null; then break; fi
  if docker exec "$CID" systemctl list-units &>/dev/null; then break; fi
  sleep 1
done

docker exec "$CID" bash -c '
  set -uo pipefail
  FAIL=0
  step() { echo ""; echo "=== $1 ==="; }
  ok()   { echo "  PASS: $1"; }
  bad()  { echo "  FAIL: $1"; FAIL=1; }

  step "1. Install RPM (unit is shipped as a package-owned file)"
  # 纯后端包无 GUI 依赖 → 直接 rpm 安装 (免 dnf 联网解析)。
  rpm -ivh --nodeps /tmp/wrouter.rpm >/dev/null 2>&1 && ok "rpm install ok" || bad "rpm install failed"
  rpm -qa | grep -i "^wrouter" >/dev/null 2>&1 || bad "package not installed"
  SERVICE_FILE=/usr/lib/systemd/user/wrouter.service
  if [ -f "$SERVICE_FILE" ]; then ok "unit installed: $SERVICE_FILE"; else bad "unit not installed"; fi
  grep -q "/usr/bin/wr-backend serve" "$SERVICE_FILE" \
    && ok "ExecStart points at /usr/bin/wr-backend" || bad "ExecStart does not point at /usr/bin/wr-backend"
  grep -q "WROUTER_FRONTEND_DIR=/usr/share/wrouter/frontend" "$SERVICE_FILE" \
    && ok "unit sets WROUTER_FRONTEND_DIR" || bad "unit missing WROUTER_FRONTEND_DIR"

  step "2. Create user + enable linger (starts systemd --user manager)"
  useradd -m wrtest 2>/dev/null || true
  # 最小容器缺用户会话总线与进程工具 —— 补齐(真实桌面机自带)。
  # 加 timeout:dnf 走联网仓库,网络不可达时会长时间挂起,冒烟不应被它拖死。
  if command -v timeout &>/dev/null; then
    timeout 180 dnf install -y -q dbus procps-ng >/dev/null 2>&1 || true
  else
    dnf install -y -q dbus procps-ng >/dev/null 2>&1 || true
  fi
  if systemctl start systemd-logind 2>/dev/null && loginctl enable-linger wrtest 2>/dev/null; then
    ok "linger enabled (loginctl)"
  else
    # 容器内 logind 常不可用 —— 手工建 linger 标记(enable-linger 的等效动作)。
    mkdir -p /var/lib/systemd/linger && touch /var/lib/systemd/linger/wrtest \
      && ok "linger enabled (manual marker; loginctl unavailable in container)" \
      || bad "enable-linger failed"
  fi
  UID_WRTEST=$(id -u wrtest)
  # linger 只保证"下次登录"起用户管理器,容器里显式拉起 user@UID.service。
  systemctl start "user@$UID_WRTEST.service" 2>/dev/null || true
  for _ in $(seq 1 40); do
    [ -S "/run/user/$UID_WRTEST/bus" ] && break
    sleep 0.5
  done
  [ -S "/run/user/$UID_WRTEST/bus" ] && ok "user bus ready" || bad "/run/user/$UID_WRTEST/bus missing"
  # wrtest 读用户日志需要 systemd-journal 组。
  usermod -aG systemd-journal wrtest 2>/dev/null || true

  step "3. systemctl --user enable (开机自启: WantedBy=default.target)"
  su wrtest -c "export XDG_RUNTIME_DIR=/run/user/$UID_WRTEST DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$UID_WRTEST/bus; \
    systemctl --user daemon-reload && systemctl --user enable wrouter.service && systemctl --user is-enabled wrouter.service" \
    && ok "service enabled" || bad "systemctl --user enable failed"

  step "4. systemctl --user start + health check"
  su wrtest -c "export XDG_RUNTIME_DIR=/run/user/$UID_WRTEST DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$UID_WRTEST/bus; \
    systemctl --user start wrouter.service && systemctl --user is-active wrouter.service" \
    && ok "service active" || bad "service failed to start"
  sleep 2
  if curl -fsS http://127.0.0.1:3462/api/v1/health 2>/dev/null | grep -q "\"ok\":true"; then
    ok "management /api/v1/health ok (3462)"
  else
    bad "management health check failed"
  fi
  if curl -fsS http://127.0.0.1:3460/health 2>/dev/null | grep -q "\"ok\":true"; then
    ok "gateway /health ok (3460)"
  else
    bad "gateway health check failed"
  fi

  # 20261102_01 P2:管理面托管前端 SPA → 浏览器直接可达 (纯后端交付的核心验收)。
  if curl -fsS http://127.0.0.1:3462/ 2>/dev/null | grep -qi "<div id=\"root\">"; then
    ok "management serves SPA index.html at / (browser reachable)"
  else
    bad "management did not serve SPA index.html at /"
  fi
  # SPA 深链回落
  if curl -fsS http://127.0.0.1:3462/settings/network 2>/dev/null | grep -qi "<div id=\"root\">"; then
    ok "SPA fallback works on deep link"
  else
    bad "SPA fallback broken on deep link"
  fi
  # session 端点 (浏览器取令牌)
  if curl -fsS http://127.0.0.1:3462/api/wr/session 2>/dev/null | grep -q "token"; then
    ok "/api/wr/session returns token (browser auth bootstrap)"
  else
    bad "/api/wr/session did not return token"
  fi
  # 数据目录: systemd 用户单元用 %h/.wrouter, 与手跑后端一致 (TD-ARCH-002)
  if su wrtest -c "test -d ~/.wrouter"; then
    ok "data dir ~/.wrouter created under the service account"
  else
    bad "data dir ~/.wrouter missing"
  fi

  step "5. Kill process -> Restart=on-failure must revive it"
  su wrtest -c "export XDG_RUNTIME_DIR=/run/user/$UID_WRTEST; pkill -9 -x wr-backend || true"
  sleep 6
  su wrtest -c "export XDG_RUNTIME_DIR=/run/user/$UID_WRTEST DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$UID_WRTEST/bus; \
    systemctl --user is-active wrouter.service" \
    | grep -q active && ok "service auto-restarted" || bad "service did not auto-restart"

  step "6. journalctl --user has service logs"
  su wrtest -c "export XDG_RUNTIME_DIR=/run/user/$UID_WRTEST DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$UID_WRTEST/bus; \
    journalctl --user -u wrouter.service -n 10 --no-pager" | head -8
  SU_LOGS=$(su wrtest -c "export XDG_RUNTIME_DIR=/run/user/$UID_WRTEST DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$UID_WRTEST/bus; \
    journalctl --user -u wrouter.service --no-pager 2>/dev/null | wc -l")
  [ "${SU_LOGS:-0}" -gt 0 ] && ok "journald captured $SU_LOGS lines" || bad "no journald logs for user unit"

  step "7. systemctl --user stop"
  su wrtest -c "export XDG_RUNTIME_DIR=/run/user/$UID_WRTEST DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$UID_WRTEST/bus; \
    systemctl --user stop wrouter.service && systemctl --user is-active wrouter.service || true" | grep -q "inactive" \
    && ok "service stopped cleanly" || bad "stop failed"

  echo ""
  if [ "$FAIL" = "0" ]; then
    echo "SYSTEMD SMOKE PASS"
  else
    echo "SYSTEMD SMOKE FAIL"
    exit 1
  fi
'

echo "[smoke-systemd] PASS"
