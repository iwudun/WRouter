# WRouter Linux RPM spec(20261102_01 P2-2)
#
# 路线:rpmbuild + 手写 spec(构建镜像 wrouter-linux-builder:v2 内无 nfpm,
# 且不经过 fpm 的 portable ruby → 历史 "rpmbuild EVP_md2" 坑不复现)。
#
# 交付物:单静态二进制(musl)+ 前端 SPA + systemd 用户单元。
# 无 Electron / WebKitGTK / GPU / 沙箱 / FUSE —— Linux 侧彻底砍掉桌面壳。
#
# 构建(容器内,ext4;避开 NTFS 挂载卷 rename 坑):
#   rpmbuild -bb --define "_topdir /tmp/rpmbuild" \
#             --define "_wr_version 0.2.1" \
#             --define "_wr_stage /tmp/stage" \
#             packaging/wrouter.spec
#
# _wr_stage 由 packaging/build-linux.sh 装配(宿主构建产物 → 容器内 ext4 暂存):
#   wr-backend              静态单二进制
#   frontend/               前端 dist(index.html + assets/)
#   wrouter.service         systemd 用户单元
#   wrouter-capabilities.json  管理面能力契约
#   build-info.json         构建清单
#   after-install.sh / after-remove.sh  安装/卸载钩子

%{!?_wr_version:%define _wr_version 0.0.0}
%{!?_wr_stage:%define _wr_stage /tmp/stage}

Name:           wrouter
Version:        %{_wr_version}
Release:        1%{?dist}
Summary:        WRouter - local AI gateway (static single binary + web console)
License:        Proprietary
URL:            https://github.com/wrouter/wrouter
BuildArch:      x86_64

# 只声明真依赖:后端是 musl 静态二进制(无 glibc 依赖)→ 老系统
# (麒麟 V10 / UOS / CentOS 7 等)均可安装。
# AutoReq: no 关闭自动 ELF/scriptlet 依赖探测,避免把宿主构建环境的
# 库依赖(以及探测器自身引入的依赖)写进包。
AutoReq:        no
Requires:       systemd
Requires:       /bin/sh

%description
WRouter 本地 AI 网关。

Linux 侧交付形态为单个 musl 静态二进制(wr-backend)+ 内置前端 SPA +
systemd 用户服务,不依赖 Electron / WebKitGTK / GPU / 沙箱 / FUSE。
管理面通过浏览器访问 http://127.0.0.1:3462/ ,网关面监听 http://127.0.0.1:3460/ 。
数据目录为 ~/.wrouter/ ,与手跑后端、桌面端共用同一约定。

%prep

%build

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}%{_bindir}
mkdir -p %{buildroot}%{_datadir}/wrouter/frontend
mkdir -p %{buildroot}%{_datadir}/wrouter/contracts
mkdir -p %{buildroot}%{_prefix}/lib/systemd/user
mkdir -p %{buildroot}%{_libexecdir}/wrouter

# 单二进制(musl 静态,已 strip)
install -m 0755 %{_wr_stage}/wr-backend %{buildroot}%{_bindir}/wr-backend

# 前端 SPA(静态托管;后端零依赖手写静态资源服务)
cp -a %{_wr_stage}/frontend/. %{buildroot}%{_datadir}/wrouter/frontend/

# 管理面能力契约(缺失时后端降级到内联默认值,不影响启动)
install -m 0644 %{_wr_stage}/wrouter-capabilities.json \
  %{buildroot}%{_datadir}/wrouter/contracts/wrouter-capabilities.json

# 构建清单(版本 / 提交 / 构建时间,便于现场排查)
install -m 0644 %{_wr_stage}/build-info.json %{buildroot}%{_datadir}/wrouter/build-info.json

# systemd 用户单元:直接由 rpm 拥有(%files),卸载时随包删除 ——
# 不再依赖安装钩子拷贝(旧 Electron 包的遗留做法)。
install -m 0644 %{_wr_stage}/wrouter.service \
  %{buildroot}%{_prefix}/lib/systemd/user/wrouter.service

# 安装/卸载钩子
install -m 0755 %{_wr_stage}/after-install.sh \
  %{buildroot}%{_libexecdir}/wrouter/after-install.sh
install -m 0755 %{_wr_stage}/after-remove.sh \
  %{buildroot}%{_libexecdir}/wrouter/after-remove.sh

%files
%dir %{_datadir}/wrouter
%dir %{_datadir}/wrouter/contracts
%{_datadir}/wrouter/contracts/wrouter-capabilities.json
%{_datadir}/wrouter/build-info.json
# 目录项 → RPM 递归收录其下全部文件(index.html + assets/*)
%{_datadir}/wrouter/frontend
%{_bindir}/wr-backend
%{_prefix}/lib/systemd/user/wrouter.service
%{_libexecdir}/wrouter/after-install.sh
%{_libexecdir}/wrouter/after-remove.sh

%post
# 仅打印提示:root 身份、无用户会话,无法代用户启用用户级服务
# (systemctl --user 在 %post 里不可用,故不在此调用)。
/bin/sh %{_libexecdir}/wrouter/after-install.sh || :

%preun
# $1=0 才是真正卸载(升级时为 1,此时不能停服务)。
if [ "$1" = "0" ]; then
  /bin/sh %{_libexecdir}/wrouter/after-remove.sh || :
fi
:

%changelog
* Tue Nov 03 2026 WRouter Team - %{_wr_version}-1
- 20261102_01 P2: Linux 交付改造为纯后端(单 musl 静态二进制 + 前端 SPA +
  systemd 用户单元),移除 Electron / WebKitGTK / GPU / 沙箱 / FUSE 依赖。
