#!/usr/bin/env bash
# AIVX 服务器安装脚本（P9，ADR-030 部署阶段项）。
# 在目标服务器上执行：解包 bundle → /opt/aivx → 安装 systemd → 启动。
# 凭据不入库（AGENTS.md Never-2）——本脚本只做无凭据操作。
set -euo pipefail

DEST=/opt/aivx
SERVICE_SRC=aivx.service
SERVICE_DST=/etc/systemd/system/aivx.service

echo "==> 安装到 ${DEST}"
mkdir -p "${DEST}"
# bundle 布局：./aivx/{aivx, static/} + ./aivx.service + ./install.sh
install -m 0755 aivx/aivx "${DEST}/aivx"
mkdir -p "${DEST}/static"
cp -r aivx/static/. "${DEST}/static/"

echo "==> 安装 systemd 单元"
install -m 0644 "${SERVICE_SRC}" "${SERVICE_DST}"
systemctl daemon-reload
systemctl enable aivx
systemctl restart aivx

echo "==> 完成。状态："
systemctl --no-pager status aivx | head -12
