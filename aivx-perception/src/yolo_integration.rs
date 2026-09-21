//! ort YOLO 真实推理集成测试（DESIGN.md §4 / P8）——feature-gated。
//!
//! 下载真实 YOLOv8n.onnx → OrtYoloBackend 加载 → 合成帧前向 → 断言不报错。
//! 仅在 `--features ort-yolo` 下编译。**需要模型文件**（本地/有网 CI 可下载）。
//! 若模型不可下载/无 ort 运行时，测试 `#[ignore]` 跳过（诚实：真实 RUN 需模型）。

use crate::pool::{DetectorPool, EngineKey, InferBackend};

/// 下载 YOLOv8n.onnx（ultralytics 官方资产）。用 curl（std Command，无 ureq 依赖）。
fn download_model(url: &str, path: &std::path::Path) -> Result<(), String> {
    let status = std::process::Command::new("curl")
        .args(["-L", "-o"])
        .arg(path)
        .arg(url)
        .status()
        .map_err(|e| format!("curl 失败: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("curl 退出码: {status:?}"))
    }
}

/// 真实推理：下载 YOLOv8n.onnx → 加载 → 合成 640×640 NV12 帧 → 前向。
/// 断言：推理不报错（返回 Vec，可能空——纯灰帧无目标，但**必须跑通**）。
///
/// `#[ignore]`：需要真实模型文件（~5MB）+ ONNX Runtime 库。CI 无此环境时
/// 显式 `--ignored` 运行；本地/有网环境运行验证真实 ort Session 推理。
#[test]
#[ignore]
fn real_ort_yolo_runs_forward_pass() {
    // 1. 下载模型（有网环境）
    let model_dir = std::env::temp_dir().join("aivx-yolo-test");
    std::fs::create_dir_all(&model_dir).ok();
    let model_path = model_dir.join("yolov8n.onnx");
    if !model_path.exists() {
        download_model(
            // 标准 ultralytics 导出（[1,84,8400]，class-major——与 yolo_math
            // parse_output 的布局实测对齐；宿主部署在 /opt/aivx/data/models/）
            "https://raw.githubusercontent.com/JasonLin1110/ultralytics_yolov8_onnx_model/main/yolov8net.onnx",
            &model_path,
        )
        .expect("应能下载 YOLOv8n.onnx");
    }

    // 2. 加载真实 ort Session（ADR-030 的接入点——真实模型）
    let backend = crate::yolo::OrtYoloBackend::new(model_path.to_str().unwrap(), 0.4, 0.45)
        .expect("ort 应能加载 YOLOv8n.onnx");

    // 3. 合成 640×640 NV12 灰帧
    let key = EngineKey {
        model_id: "default".into(),
        device: "cpu".into(),
        input_w: 640,
        input_h: 640,
    };
    let nv12 = vec![128u8; 640 * 640 + 640 * 640 / 2];

    // 4. 真实前向：直接后端
    let dets = backend.detect(&key, std::slice::from_ref(&nv12));
    assert_eq!(dets.len(), 1, "后端应处理 1 个输入");
    let _ = &dets[0];

    // 5. 经 DetectorPool 走完整批推理路径
    let pool = std::sync::Arc::new(DetectorPool::new(backend));
    let pool_worker = pool.clone();
    std::thread::spawn(move || pool_worker.run_worker());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let pool_dets = pool.infer(&key, nv12);
    let _ = pool_dets;
    pool.shutdown();
    println!("REAL ORT YOLO: forward pass completed");
}
