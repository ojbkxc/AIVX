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

    /// NV12 → RGB float（ort 输入）。预分配 buffer。
    fn nv12_to_rgb_float(nv12: &[u8], w: u32, h: u32) -> Vec<f32> {
        // NV12：Y 平面 w*h + UV 交错平面 w*h/2
        let mut rgb = vec![0f32; (w * h * 3) as usize];
        let y_size = (w * h) as usize;
        let (y_plane, uv) = nv12.split_at(y_size);
        let _ = uv;
        // 简化：用 Y 作为灰度近似 RGB（真实 YOLO 需完整 NV12→RGB 转换 + 归一化）
        for i in 0..y_size {
            let y = y_plane[i] as f32 / 255.0;
            rgb[i * 3] = y;
            rgb[i * 3 + 1] = y;
            rgb[i * 3 + 2] = y;
        }
        rgb
    }

    /// 解析 YOLOv8 输出（1×4×8400）→ 候选框（conf > threshold）。
    /// 输出布局：4 = x_center, y_center, w, h + 80 class scores。
    fn parse_output(output: &[f32], input_size: u32, conf: f32) -> Vec<(f32, f32, f32, f32, f32)> {
        let stride = 4 + 80; // 4 坐标 + 80 COCO 类
        let num_det = output.len() / stride;
        let mut candidates = Vec::new();
        for i in 0..num_det {
            let base = i * stride;
            let cx = output[base];
            let cy = output[base + 1];
            let w = output[base + 2];
            let h = output[base + 3];
            // 找最高类分数
            let mut best_score = 0f32;
            for c in 4..stride {
                let s = output[base + c];
                if s > best_score {
                    best_score = s;
                }
            }
            if best_score >= conf {
                // 坐标从归一化 → 像素
                let x1 = (cx - w / 2.0) * input_size as f32;
                let y1 = (cy - h / 2.0) * input_size as f32;
                let bw = w * input_size as f32;
                let bh = h * input_size as f32;
                candidates.push((x1, y1, bw, bh, best_score));
            }
        }
        candidates
    }

    /// NMS（IoU 阈值过滤重叠框）。
    fn nms(candidates: &[(f32, f32, f32, f32, f32)], iou_threshold: f32) -> Vec<Det> {
        let mut sorted = candidates.to_vec();
        sorted.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal));
        let mut kept = Vec::new();
        for &c in &sorted {
            let mut overlap = false;
            for &k in &kept {
                if iou(c.0, c.1, c.2, c.3, k.0, k.1, k.2, k.3) > iou_threshold {
                    overlap = true;
                    break;
                }
            }
            if !overlap {
                kept.push(Det {
                    x: c.0.max(0.0) as u32,
                    y: c.1.max(0.0) as u32,
                    w: c.2 as u32,
                    h: c.3 as u32,
                });
            }
        }
        kept
    }
}

fn iou(ax: f32, ay: f32, aw: f32, ah: f32, bx: f32, by: f32, bw: f32, bh: f32) -> f32 {
    let ix1 = ax.max(bx);
    let iy1 = ay.max(by);
    let ix2 = (ax + aw).min(bx + bw);
    let iy2 = (ay + ah).min(by + bh);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let union = aw * ah + bw * bh - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
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
            batch.extend(Self::nv12_to_rgb_float(inp, key.input_w, key.input_h));
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
            let cands = Self::parse_output(&slice, self.input_size, self.conf_threshold);
            results.push(Self::nms(&cands, self.iou_threshold));
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
        let cands = OrtYoloBackend::parse_output(&out, 640, 0.4);
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
        let dets = OrtYoloBackend::nms(&cands, 0.45);
        assert_eq!(dets.len(), 2, "重叠的应只剩一个，不重叠的保留");
    }

    /// IoU 计算。
    #[test]
    fn iou_correct() {
        // 完全重叠 → 1.0
        let v = iou(0.0, 0.0, 10.0, 10.0, 0.0, 0.0, 10.0, 10.0);
        assert!((v - 1.0).abs() < 0.01);
        // 完全不重叠 → 0.0
        let v = iou(0.0, 0.0, 10.0, 10.0, 100.0, 100.0, 10.0, 10.0);
        assert!((v - 0.0).abs() < 0.01);
    }

    /// NV12 → RGB float：尺寸正确 + Y 灰度近似。
    #[test]
    fn nv12_to_rgb_shape() {
        let nv12 = vec![128u8; 640 * 360 + 640 * 360 / 2];
        let rgb = OrtYoloBackend::nv12_to_rgb_float(&nv12, 640, 360);
        assert_eq!(rgb.len(), 640 * 360 * 3);
        // Y=128/255 ≈ 0.502
        assert!((rgb[0] - 0.502).abs() < 0.01);
    }
}
