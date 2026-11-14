#!/bin/bash
# WRouter RPM 安装后钩子(20261102_01 P2-6:Linux 交付改为纯后端)。
#
# 由 packaging/wrouter.spec 的 %post 段调用。
#
# 注意:
#   - systemd 单元由 rpm %files 直接拥有(/usr/lib/systemd/user/wrouter.service),
#     不再由本脚本拷贝(旧 Electron 包的遗留做法)—— 卸载时随包自动删除。
#   - 本脚本以 root 身份运行,无用户会话总线 → 无法代用户执行 systemctl --user,
#     因此只打印提示,不代为启用服务。

set -euo pipefail

echo ""
echo "=== WRouter 安装完成 ==="
echo "Web 管理面: http://127.0.0.1:3462"
echo "网关面:     http://127.0.0.1:3460"
echo "数据目录:   ~/.wrouter/ (与手跑后端共用)"
echo ""
echo "启用用户服务(在登录会话中执行):"
echo "  systemctl --user enable --now wrouter.service"
echo "查看状态: systemctl --user status wrouter.service"
echo "查看日志: journalctl --user -u wrouter.service -f"
echo ""
echo "20261102_01 P6:管理面「设置 → 通用 → 服务控制」可暂停/恢复网关面"
echo "(客户端会连接失败),也可直接停止服务;重新拉起:"
echo "  systemctl --user start wrouter.service"
echo ""
echo "卸载命令: rpm -e wrouter"
echo "=========================="
