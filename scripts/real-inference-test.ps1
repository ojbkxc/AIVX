# AIVX 真实推理端到端验证脚本（Windows 版）。
# 用法: powershell -ExecutionPolicy Bypass -File scripts/real-inference-test.ps1

Write-Host "=== AIVX 真实推理端到端验证 ===" -ForegroundColor Cyan

# 1. 检查 cargo
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "缺少 cargo（Rust 工具链）" -ForegroundColor Red
    exit 1
}

# 2. 编译 ort-yolo feature 并跑真实推理测试
Write-Host "编译 ort-yolo feature 并跑真实推理测试（首次编译 ONNX Runtime 较慢）..."
cargo test -p aivx-perception --features ort-yolo `
  yolo_integration::real_ort_yolo_runs_forward_pass `
  -- --ignored --test-threads=1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "`n✅ 真实 ort Session 推理已跑通（REAL ORT YOLO: forward pass completed）" -ForegroundColor Green
Write-Host "下一步：接入真实 TP-LINK 摄像头（见 DEPLOYMENT.md 端到端步骤）"