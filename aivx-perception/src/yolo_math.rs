//! YOLO 纯数学函数（DESIGN.md §4 / P8）——无 ort 依赖，CI 必跑。
//!
//! 输出解析 / NMS / IoU / NV12→RGB 预处理都是纯函数，不依赖模型加载。
//! 与 `ort-yolo` feature 解耦：这些测试在 CI 无需下载模型也能机器验证。
//! `OrtYoloBackend`（yolo.rs，feature gate）用这些函数做真实推理。

use crate::pool::Det;

/// 解析 YOLOv8 输出（1×4×8400）→ 候选框（conf > threshold）。
/// 输出布局：4 = x_center, y_center, w, h + 80 class scores。
pub fn parse_output(output: &[f32], input_size: u32, conf: f32) -> Vec<(f32, f32, f32, f32, f32)> {
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

/// IoU 计算。
pub fn iou(ax: f32, ay: f32, aw: f32, ah: f32, bx: f32, by: f32, bw: f32, bh: f32) -> f32 {
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

/// NMS（IoU 阈值过滤重叠框）。
pub fn nms(candidates: &[(f32, f32, f32, f32, f32)], iou_threshold: f32) -> Vec<Det> {
    let mut sorted = candidates.to_vec();
    sorted.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<Det> = Vec::new();
    for &c in &sorted {
        let mut overlap = false;
        for &k in &kept {
            if iou(
                c.0, c.1, c.2, c.3, k.x as f32, k.y as f32, k.w as f32, k.h as f32,
            ) > iou_threshold
            {
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

/// NV12 → RGB float（ort 输入）。Y 平面灰度近似（真实 YOLO 需完整转换）。
pub fn nv12_to_rgb_float(nv12: &[u8], w: u32, h: u32) -> Vec<f32> {
    let mut rgb = vec![0f32; (w * h * 3) as usize];
    let y_size = (w * h) as usize;
    let (y_plane, _uv) = nv12.split_at(y_size);
    for i in 0..y_size {
        let y = y_plane[i] as f32 / 255.0;
        rgb[i * 3] = y;
        rgb[i * 3 + 1] = y;
        rgb[i * 3 + 2] = y;
    }
    rgb
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
        let cands = parse_output(&out, 640, 0.4);
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
        let dets = nms(&cands, 0.45);
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
        let rgb = nv12_to_rgb_float(&nv12, 640, 360);
        assert_eq!(rgb.len(), 640 * 360 * 3);
        // Y=128/255 ≈ 0.502
        assert!((rgb[0] - 0.502).abs() < 0.01);
    }
}
