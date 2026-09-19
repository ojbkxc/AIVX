//! ort YOLO 真实推理集成测试（DESIGN.md §4 / P8）——feature-gated。
//!
//! 这是"真实 ort Session 推理 RUN 过"的机器验证：
//! 下载真实 YOLOv8n.onnx → OrtYoloBackend 加载 → 合成帧前向 → 断言不报错。
//! 仅在 `--features ort-yolo` 下编译（CI 显式运行）。

use crate::pool::{DetectorPool, EngineKey, InferBackend};

/// 下载 YOLOv8n.onnx（ultralytics 官方资产）。失败返回描述。
/// 用 `ureq`（ort 的传递依赖，feature 下可用）——数据面 std-only 约束不破。
fn download_model(url: &str, path: &std::path::Path) -> Result<(), String> {
    let body = ureq::get(url)
        .call()
        .map_err(|e| format!("下载模型失败: {e}"))?
        .into_body()
        .read_to_vec()
        .map_err(|e| format!("读取响应失败: {e}"))?;
    std::fs::write(path, body).map_err(|e| format!("写模型失败: {e}"))
}

/// 真实推理：下载 YOLOv8n.onnx → 加载 → 合成 640×640 NV12 帧 → 前向。
/// 断言：推理不报错（返回 Vec，可能空——纯灰帧无目标，但**必须跑通**）。
#[test]
fn real_ort_yolo_runs_forward_pass() {
    // 1. 下载模型（CI 网络可用；本地已缓存跳过）
    let model_dir = std::env::temp_dir().join("aivx-yolo-test");
    std::fs::create_dir_all(&model_dir).ok();
    let model_path = model_dir.join("yolov8n.onnx");
    if !model_path.exists() {
        download_model(
            "https://github.com/ultralytics/assets/releases/download/v0.0.0/yolov8n.onnx",
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

    // 4. 真实前向：DetectorPool 或直接后端
    let dets = backend.detect(&key, std::slice::from_ref(&nv12));
    assert_eq!(dets.len(), 1, "后端应处理 1 个输入");
    // 纯灰帧可能 0 目标，但**推理必须成功返回**（不 panic/不 Err）——这就是
    // "真实 ort Session 推理 RUN 过"的机器证据。
    let _ = &dets[0];

    // 5. 经 DetectorPool 走完整批推理路径
    let pool = DetectorPool::new(backend);
    let pool = std::sync::Arc::new(pool);
    let pool_worker = pool.clone();
    std::thread::spawn(move || pool_worker.run_worker());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let pool_dets = pool.infer(&key, nv12);
    // worker 走真实 ort 前向——同样必须成功返回（可能空但非 panic）
    let _ = pool_dets;
    pool.shutdown();
    println!("REAL ORT YOLO: forward pass completed");
}
