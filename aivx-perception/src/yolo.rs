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
///
/// ort 2.0.0-rc.13 API（按 docs.rs 验证）：
/// - `Session::builder()?` 返回 Result<SessionBuilder>
/// - `.commit_from_file(path)` 加载模型（非 with_model_from_file）
/// - `session.run(...)` 取 `&mut self`（内部非线程安全），输入用 `inputs!` 宏
pub struct OrtYoloBackend {
    /// model_id → ort Session（ADR-025 同键共享）。Mutex 保证 run 的 &mut。
    sessions: Mutex<HashMap<String, ort::session::Session>>,
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
        let session = ort::session::Session::builder()
            .map_err(|e| format!("ort builder 失败: {e}"))?
            .commit_from_file(model_path)
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

    /// 模型是否已加载（session_for 的存在性检查——锁在 detect 内完成）。
    fn has_model(&self, model_id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(model_id)
    }
}

impl InferBackend for OrtYoloBackend {
    fn detect(&self, key: &EngineKey, inputs: &[Vec<u8>]) -> Vec<Vec<Det>> {
        if !self.has_model(&key.model_id) {
            return inputs.iter().map(|_| Vec::new()).collect();
        }
        // 批输入：每个 NV12 → RGB float，shape [N, 3, 640, 640]
        let n = inputs.len();
        let mut batch = Vec::with_capacity(n * 3 * 640 * 640);
        for inp in inputs {
            batch.extend(nv12_to_rgb_float(inp, key.input_w, key.input_h));
        }
        // ort rc.13 实测 API（对 2.0.0-rc.13 源码核对）：
        // - from_array 接受 (shape, data)（shape 在前）；(Vec, [i64;4]) 顺序反了
        //   不实现 OwnedTensorArrayData
        // - run 输入 SessionInputs：From<Vec<(K,V)>> / HashMap，不接受
        //   Vec<Value>——用 (输入名, 张量) 元组 Vec
        // - try_extract_tensor 返回 Result<(&Shape, &[T])>——数据在 .1
        // MutexGuard::map 是 unstable（mapped_lock_guards）——直接持 map 锁
        // 跑完 run（单模型串行，无并发损失）。
        let shape = [n as i64, 3, 640, 640];
        let empty = || inputs.iter().map(|_| Vec::new()).collect::<Vec<Vec<_>>>();
        let tensor = match ort::value::Value::from_array((shape, batch)) {
            Ok(v) => v,
            Err(_) => return empty(),
        };
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(&key.model_id) else {
            return empty();
        };
        // 输入名从 session 元数据动态取（第 1 个输入；YOLOv8 导出为
        // "images" 但不自绑名字——适配任意导出工具的命名）。
        let input_name = match session.inputs().first() {
            Some(i) => i.name().to_string(),
            None => return empty(),
        };
        let outputs = match session.run(vec![(input_name, tensor)]) {
            Ok(o) => o,
            Err(_) => return empty(),
        };
        // 每个输入解析其输出切片。真实模型输出 [n, 84, 8400]（class-major），
        // 摊平后第 i 个输入占 [i*84*8400, (i+1)*84*8400)。
        let output = match outputs[0].try_extract_tensor::<f32>() {
            Ok((_, data)) => data,
            Err(_) => return empty(),
        };
        let per_input = output.len() / n.max(1);
        // 模型空间 → 帧空间缩放（letterbox：x 方向 640/input_w）
        let scale_x = self.input_size as f32 / key.input_w as f32;
        let scale_y = self.input_size as f32 / key.input_h as f32;
        let mut results = Vec::with_capacity(n);
        for i in 0..n {
            let slice: Vec<f32> = output[i * per_input..(i + 1) * per_input].to_vec();
            let num_det = slice.len() / 84; // 4 坐标 + 80 类（按 84 整除）
            let cands = parse_output(&slice, num_det, self.conf_threshold, scale_x, scale_y);
            results.push(nms(&cands, self.iou_threshold));
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 批前向的输出切片拆分：[n, 84, 8400] 按输入边界拆分（yolo_math 的
    /// parse_output 在 yolo_math 模块已有布局测试，此处验证拆分数学本身）。
    #[test]
    fn per_input_slice_splits_batch_output() {
        // 模拟 2 输入 × 84×3 的摊平输出
        let total = 2 * 84 * 3;
        let output: Vec<f32> = (0..total).map(|i| i as f32).collect();
        let n = 2;
        let per_input = output.len() / n;
        let s0 = &output[0..per_input];
        let s1 = &output[per_input..2 * per_input];
        assert_eq!(s0[0], 0.0);
        assert_eq!(s1[0], per_input as f32, "第 2 个输入切片应从 per_input 起");
    }
}
