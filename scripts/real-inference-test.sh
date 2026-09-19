#!/usr/bin/env bash
# AIVX 真实推理端到端验证脚本（DESIGN.md ADR-030 的部署阶段项）。
#
# 在**你的机器**（有网 + 有 CPU/GPU）上运行，验证真实 ort Session 推理：
#   1. 下载 YOLOv8n.onnx（~5MB，ultralytics 官方资产）
#   2. 编译 ort-yolo feature（需 ONNX Runtime 原生库）
#   3. 跑真实推理测试：加载模型 → 合成帧前向 → DetectorPool 批路径
#
# 用法:
#   ./scripts/real-inference-test.sh
# 或（本机无 curl）:
#   powershell -File scripts/real-inference-test.ps1
#
# 退出码: 0 = 真实推理跑通（"REAL ORT YOLO: forward pass completed"）
#         非0 = 失败（看输出定位）

set -euo pipefail

echo "=== AIVX 真实推理端到端验证 ==="
echo "1/3 检查环境..."
command -v cargo >/dev/null || { echo "缺少 cargo（Rust 工具链）"; exit 1; }

echo "2/3 编译 ort-yolo feature 并跑真实推理测试（首次编译 ONNX Runtime 可能较慢）..."
# yolo_integration::real_ort_yolo_runs_forward_pass 会下载 YOLOv8n.onnx 到临时目录，
# 加载真实模型 → 合成 640×640 NV12 帧 → DetectorPool 批路径前向。
cargo test -p aivx-perception --features ort-yolo \
  yolo_integration::real_ort_yolo_runs_forward_pass \
  -- --ignored --test-threads=1

echo "3/3 完成。"
echo "✅ 真实 ort Session 推理已跑通（REAL ORT YOLO: forward pass completed）"
echo "下一步：接入真实 TP-LINK 摄像头（见 DEPLOYMENT.md 端到端步骤）"