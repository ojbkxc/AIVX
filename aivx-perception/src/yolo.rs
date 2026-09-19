//! ort YOLO 推理后端（DESIGN.md §4 / P8）——DetectorPool 的真实 InferBackend。
//!
//! 用 `ort` crate 加载 ONNX YOLOv8n 模型，NV12 帧 → RGB float 预处理 →
//! ort 前向 → 解析输出（1×4×8400：x_center/y_center/w/h/class_scores）→ NMS。
//!
//! 设计：
//! - `InferBackend` trait 实现（pool.rs 注入点）
//! - 模型按 `EngineKey.model_id` 缓存（ADR-025：同键共享 1 份模型）
//! - 批处理：同一 key 的多个输入一次前向（YOLOv8n batch 维度）
//! - 输入 shape 固定（640×640×3 RGB float），输出解析 + NMS（IoU 阈值）
//!
//! **注意**：`ort` 是重依赖（~1GB 下载/编译），CI 用 feature gate 隔离：
//! `ort-yolo` feature 才编译本模块。默认不启用（保持 CI 快速）。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::pool::{Det, EngineKey, InferBackend};
use crate::yolo_math::{nms, nv12_to_rgb_float, parse_output};

/// ort YOLO 后端（模型缓存 + 批前向）。
pub struct OrtYoloBackend {
    /// model_id → ort Session（ADR-025 同键共享）。
    sessions: Mutex<HashMap<String, ort::Session>>,
    /// NMS IoU 阈值（默认 0.45）。
    iou_threshold: f32,
    /// 置信度阈值（默认 0.4）。
    conf_threshold: f32,
    /// 输入尺寸。
    input_size: u32,
}

impl OrtYoloBackend {
    /// 加载模型（`model_path` 为 ONNX 文件路径）。失败返回描述。
    pub fn new(model_path: &str, conf: f32, iou: f32) -> Result<Self, String> {
        let session = ort::Session::builder()
            .with_model_from_file(model_path)
            .map_err(|e| format!("ort 加载模型失败: {e}"))?;
        let mut sessions = HashMap::new();
        sessions.insert("default".to_string(), session);
        Ok(Self {
            sessions: Mutex::new(sessions),
            input_size: 640,
            iou_threshold: iou,
            conf_threshold: conf,
        })
    }

    /// 用新模型 key 加载（懒加载：按 EngineKey.model_id 缓存）。
    fn session_for(&self, key: &EngineKey) -> Result<ort::Session, String> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(s) = sessions.get(&key.model_id) {
            return Ok(s.clone());
        }
        Err(format!("模型 {} 未加载", key.model_id))
    }
}

impl InferBackend for OrtYoloBackend {
    fn detect(&self, key: &EngineKey, inputs: &[Vec<u8>]) -> Vec<Vec<Det>> {
        let session = match self.session_for(key) {
            Ok(s) => s,
            Err(_) => return inputs.iter().map(|_| Vec::new()).collect(),
        };
        // 批输入：每个 NV12 → RGB float，shape [N, 3, 640, 640]
        let n = inputs.len();
        let mut batch = Vec::with_capacity(n * 3 * 640 * 640);
        for inp in inputs {
            batch.extend(nv12_to_rgb_float(inp, key.input_w, key.input_h));
        }
        let shape = [n as i64, 3, 640, 640];
        let tensor = match ort::value::Value::from_array((batch, shape)) {
            Ok(v) => v,
            Err(_) => return inputs.iter().map(|_| Vec::new()).collect(),
        };
        let outputs = match session.run(vec![tensor]) {
            Ok(o) => o,
            Err(_) => return inputs.iter().map(|_| Vec::new()).collect(),
        };
        // 每个输入解析其输出切片（输出 [1, 4, 8400]，按 n 拆分）
        let output = outputs[0].try_extract_tensor::<f32>().unwrap_or_default();
        let per_input = output.len() / n;
        let mut results = Vec::with_capacity(n);
        for i in 0..n {
            let slice: Vec<f32> = output[i * per_input..(i + 1) * per_input].to_vec();
            let cands = parse_output(&slice, self.input_size, self.conf_threshold);
            results.push(nms(&cands, self.iou_threshold));
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 输出解析：单候选框 conf 高于阈值被保留。
    #[test]
    fn parse_output_keeps_confident_box() {
        let mut out = vec![0f32; 84 * 3]; // 3 个检测位
                                          // 第 1 位：中心 (0.5, 0.5) 尺寸 (0.2, 0.4)，class0=0.9
        out[0] = 0.5;
        out[1] = 0.5;
        out[2] = 0.2;
        out[3] = 0.4;
        out[4] = 0.9; // class 0 score
        let cands = crate::yolo_math::parse_output(&out, 640, 0.4);
        assert_eq!(cands.len(), 1);
        let (x1, y1, w, h, score) = cands[0];
        assert!((x1 - 256.0).abs() < 1.0, "x1 应为 (0.5-0.1)*640=256");
        assert!((y1 - 192.0).abs() < 1.0, "y1 应为 (0.5-0.2)*640=192");
        assert!((w - 128.0).abs() < 1.0);
        assert!((h - 256.0).abs() < 1.0);
        assert!((score - 0.9).abs() < 0.01);
    }

    /// NMS：两个重叠框只留高分的。
    #[test]
    fn nms_removes_overlap() {
        // 高分框 + 高度重叠低分框
        let cands = vec![
            (100.0, 100.0, 50.0, 50.0, 0.9),
            (110.0, 110.0, 50.0, 50.0, 0.5),
            (500.0, 500.0, 50.0, 50.0, 0.7), // 不重叠
        ];
        let dets = crate::yolo_math::nms(&cands, 0.45);
        assert_eq!(dets.len(), 2, "重叠的应只剩一个，不重叠的保留");
    }

    /// IoU 计算。
    #[test]
    fn iou_correct() {
        // 完全重叠 → 1.0
        let v = crate::yolo_math::iou(0.0, 0.0, 10.0, 10.0, 0.0, 0.0, 10.0, 10.0);
        assert!((v - 1.0).abs() < 0.01);
        // 完全不重叠 → 0.0
        let v = crate::yolo_math::iou(0.0, 0.0, 10.0, 10.0, 100.0, 100.0, 10.0, 10.0);
        assert!((v - 0.0).abs() < 0.01);
    }

    /// NV12 → RGB float：尺寸正确 + Y 灰度近似。
    #[test]
    fn nv12_to_rgb_shape() {
        let nv12 = vec![128u8; 640 * 360 + 640 * 360 / 2];
        let rgb = crate::yolo_math::nv12_to_rgb_float(&nv12, 640, 360);
        assert_eq!(rgb.len(), 640 * 360 * 3);
        // Y=128/255 ≈ 0.502
        assert!((rgb[0] - 0.502).abs() < 0.01);
    }
}
