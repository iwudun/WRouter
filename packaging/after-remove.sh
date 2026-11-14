#!/bin/bash
# WRouter RPM 卸载后钩子(20261102_01 P2-6:Linux 交付改为纯后端)。
#
# 由 packaging/wrouter.spec 的 %preun 段调用(仅真正卸载时,升级时不执行)。
#
# 注意:
#   - systemd 单元由 rpm %files 直接拥有 → 卸载时随包自动删除,无需手工 rm。
#   - 本脚本以 root 身份运行 → systemctl --user 无用户会话总线,尽力而为即可。

set -euo pipefail

# 尽力停用用户服务(root 上下文通常无用户总线,失败可忽略)
if command -v systemctl &>/dev/null; then
  systemctl --user disable --now wrouter.service 2>/dev/null || true
  systemctl --user daemon-reload 2>/dev/null || true
fi

echo ""
echo "=== WRouter 已卸载 ==="
echo "用户数据保留在 ~/.wrouter/"
echo "如需完全清除: rm -rf ~/.wrouter"
echo "========================"
