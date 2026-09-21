//! YOLO 纯数学函数（DESIGN.md §4 / P8）——无 ort 依赖，CI 必跑。
//!
//! 输出解析 / NMS / IoU / NV12→RGB 预处理都是纯函数，不依赖模型加载。
//! 与 `ort-yolo` feature 解耦：这些测试在 CI 无需下载模型也能机器验证。
//! `OrtYoloBackend`（yolo.rs，feature gate）用这些函数做真实推理。

use crate::pool::Det;

/// 解析 YOLOv8 输出 → 候选框（conf > threshold）。
///
/// 布局对真实 ultralytics 导出 yolov8n.onnx（[1, 84, 8400]）实测验证：
/// **class-major** 摊平（index = c * num_det + d），不是 det-major——
/// 检测 d 的 4 坐标在 output[d] / output[num_det+d] / …，类分数在
/// output[(4+c)*num_det + d]。坐标是模型输入空间（640×640）的**像素值**
/// （实测范围 2.7~637），中心格式 cx/cy/w/h——不再是归一化（旧实现的
/// `*input_size` 放大是 bug）。
///
/// `scale_x/scale_y`：模型 640 空间 → 帧空间缩放（720p 帧则 2.0 / 1.125）。
/// 返回左上角格式（x1/y1/w/h）——nms 的 iou 按此约定。
pub fn parse_output(
    output: &[f32],
    num_det: usize,
    conf: f32,
    scale_x: f32,
    scale_y: f32,
) -> Vec<(f32, f32, f32, f32, f32)> {
    if num_det == 0 {
        return Vec::new();
    }
    let num_classes = output.len() / num_det - 4;
    let mut candidates = Vec::new();
    for d in 0..num_det {
        let cx = output[d];
        let cy = output[num_det + d];
        let w = output[2 * num_det + d];
        let h = output[3 * num_det + d];
        // 找最高类分数（已 sigmoid——实测灰图 max≈0.0008，有目标≈0.89）
        let mut best_score = 0f32;
        for c in 4..4 + num_classes {
            let s = output[c * num_det + d];
            if s > best_score {
                best_score = s;
            }
        }
        if best_score >= conf {
            candidates.push((
                (cx - w / 2.0) * scale_x,
                (cy - h / 2.0) * scale_y,
                w * scale_x,
                h * scale_y,
                best_score,
            ));
        }
    }
    candidates
}

/// IoU 计算。
#[allow(clippy::too_many_arguments)]
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

/// NV12 → RGB float（ort 输入，letterbox 到 640×640，**CHW 平面布局**）。
///
/// 完整 YUV→RGB（BT.601）+ 长边缩放到 640 + 短边中心补灰（letterbox 保
/// 纵横比——ultralytics 推理的输入约定）。
///
/// 返回 3×640×640 的 **CHW**（先全 R 平面、再 G、再 B）——ort 的
/// `[N,3,640,640]` 输入要求通道在外层；此前返回 HWC 交错被按 CHW 解读，
/// 通道全错位 → 模型输入成噪声 → 检出恒 0 框（线上 inferences 涨而
/// 报警恒 0 的根因，python transpose(2,0,1) 对照实验定位）。
pub fn nv12_to_rgb_float(nv12: &[u8], w: u32, h: u32) -> Vec<f32> {
    let y_size = (w * h) as usize;
    if nv12.len() < y_size * 3 / 2 || w == 0 || h == 0 {
        return vec![0f32; 640 * 640 * 3];
    }
    let (y_plane, uv) = nv12.split_at(y_size);
    // 目标：长边 640，短边按比例缩放后**居中**放置（上下/左右对称补灰）
    let scale = 640.0 / w.max(h) as f32;
    let tw = (w as f32 * scale).round().max(1.0) as usize;
    let th = (h as f32 * scale).round().max(1.0) as usize;
    let ox = (640 - tw.min(640)) / 2;
    let oy = (640 - th.min(640)) / 2;
    let mut out = vec![0.5f32; 640 * 640 * 3]; // 补灰（0.5 ≈ 128/255）
    let plane = 640 * 640; // CHW：R 平面 [0,plane)，G [plane,2*plane)，B [2*plane,3*plane)
    for dy in 0..th.min(640) {
        let sy = ((dy as f32) / scale) as usize;
        let sy = sy.min(h as usize - 1);
        for dx in 0..tw.min(640) {
            let sx = ((dx as f32) / scale) as usize;
            let sx = sx.min(w as usize - 1);
            // 最近邻取点 + BT.601 YUV→RGB
            let y = y_plane[sy * w as usize + sx] as f32;
            let vi = (sy / 2) * (w as usize / 2) + sx / 2;
            let (u, v) = if vi * 2 + 1 < uv.len() {
                (uv[vi * 2] as f32, uv[vi * 2 + 1] as f32)
            } else {
                (128.0, 128.0)
            };
            let c = y - 16.0;
            let d = u - 128.0;
            let e = v - 128.0;
            let r = (1.164 * c + 1.596 * e).clamp(0.0, 255.0) / 255.0;
            let g = (1.164 * c - 0.392 * d - 0.813 * e).clamp(0.0, 255.0) / 255.0;
            let b = (1.164 * c + 2.017 * d).clamp(0.0, 255.0) / 255.0;
            let o = (oy + dy) * 640 + ox + dx;
            out[o] = r;
            out[plane + o] = g;
            out[2 * plane + o] = b;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 输出解析（class-major 布局）：单候选框 conf 高于阈值被保留。
    ///
    /// 布局按真实 yolov8n.onnx 实测（[1,84,8400] 摊平 → c*8400+d）：
    /// 2 个检测位、4+1 类（用小类数验证索引数学，不依赖 80 类全量）。
    #[test]
    fn parse_output_keeps_confident_box() {
        let num_det = 2;
        let num_classes = 1; // 4 坐标 + 1 类 → len = 5*2
        let mut out = vec![0f32; (4 + num_classes) * num_det];
        // det 0：中心 (320, 320) 尺寸 (100, 200)——像素值（模型 640 空间）
        out[0] = 320.0;
        out[num_det] = 320.0;
        out[2 * num_det] = 100.0;
        out[3 * num_det] = 200.0;
        out[4 * num_det] = 0.9;
        // class 0 score（class-major：c=4 → 4*2+0）
        // det 1：低分——被阈值过滤
        out[1] = 100.0;
        out[num_det + 1] = 100.0;
        out[2 * num_det + 1] = 50.0;
        out[3 * num_det + 1] = 50.0;
        out[4 * num_det + 1] = 0.1;
        let cands = parse_output(&out, num_det, 0.4, 1.0, 1.0);
        assert_eq!(cands.len(), 1, "低分 det 应被阈值过滤");
        let (x1, y1, w, h, score) = cands[0];
        assert!((x1 - 270.0).abs() < 1.0, "x1 应为 320-100/2=270");
        assert!((y1 - 220.0).abs() < 1.0, "y1 应为 320-200/2=220");
        assert!((w - 100.0).abs() < 1.0);
        assert!((h - 200.0).abs() < 1.0);
        assert!((score - 0.9).abs() < 0.01);
    }

    /// 输出解析：scale 缩放（模型 640 → 720p 帧，x 方向 /640*1280）。
    #[test]
    fn parse_output_scales_to_frame_space() {
        let num_det = 1;
        let mut out = vec![0f32; 5];
        // cx / cy（num_det=1 时 cy 在 index 1）/ w / h / score
        out[0] = 320.0;
        out[1] = 180.0;
        out[2] = 64.0;
        out[3] = 32.0;
        out[4] = 0.8;
        // 640×360 模型空间 → 1280×720 帧空间：scale 2.0/2.0
        let cands = parse_output(&out, num_det, 0.4, 2.0, 2.0);
        assert_eq!(cands.len(), 1);
        let (x1, y1, w, h, _) = cands[0];
        assert!((x1 - 576.0).abs() < 1.0, "x1=(320-32)*2=576");
        assert!((y1 - 328.0).abs() < 1.0, "y1=(180-16)*2=328");
        assert!((w - 128.0).abs() < 1.0);
        assert!((h - 64.0).abs() < 1.0);
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

    /// NV12 → RGB float：输出恒为 640×640×3（模型输入 shape，**CHW**）+
    /// 灰帧 BT.601（Y=U=V=128 → RGB ≈ 130/255 ≈ 0.511）+ letterbox 居中。
    #[test]
    fn nv12_to_rgb_shape() {
        let nv12 = vec![128u8; 640 * 360 + 640 * 360 / 2];
        let rgb = nv12_to_rgb_float(&nv12, 640, 360);
        assert_eq!(rgb.len(), 640 * 640 * 3, "输出必须是模型输入 shape");
        // 360 高居中在 640：内容区行 [140, 500)。中心 (320,320) 在内容区。
        // CHW：R 平面基址 0、G 基址 plane、B 基址 2*plane。
        let plane = 640 * 640;
        let px = 320 * 640 + 320;
        assert!(
            (rgb[px] - 0.511).abs() < 0.01,
            "中心 R 应为 BT.601 灰 0.511，实得 {:.3}",
            rgb[px]
        );
        assert!(
            (rgb[plane + px] - rgb[px]).abs() < 0.001
                && (rgb[2 * plane + px] - rgb[px]).abs() < 0.001,
            "灰帧三平面应相等"
        );
        // letterbox 区（行 10 在内容区外）应为补灰 0.5
        let top = 10 * 640 + 320;
        assert!((rgb[top] - 0.5).abs() < 0.01, "letterbox 区应为 0.5 补灰");
        // 内容区首行（行 140）应是灰而非补灰
        let first = 140 * 640 + 320;
        assert!(
            (rgb[first] - 0.511).abs() < 0.01,
            "内容区首行应为灰（居中偏移=140），实得 {:.3}",
            rgb[first]
        );
    }

    /// NV12 → RGB：色度渲染（U 偏移 → B 平面变化；BT.601 中 B 与 U 正相关）。
    /// CHW 布局回归：通道错位（HWC）时 B 平面会拿到 R 数据，断言失败。
    #[test]
    fn nv12_to_rgb_chroma() {
        let w = 4u32;
        let h = 4u32;
        let mut nv12 = vec![128u8; (w * h * 3 / 2) as usize];
        // U=200（B 分量正向偏移：B = 1.164*C + 2.017*(U-128)）
        for i in ((w * h) as usize)..((w * h * 3 / 2) as usize) {
            if (i - (w * h) as usize) % 2 == 0 {
                nv12[i] = 200;
            }
        }
        let rgb = nv12_to_rgb_float(&nv12, w, h);
        // 4x4 → scale 160 → 全图 640；中心点应偏蓝（B > R）
        let plane = 640 * 640;
        let c = 320 * 640 + 320;
        assert!(
            rgb[2 * plane + c] > rgb[c] + 0.1,
            "U 高应显著偏蓝：B={:.3} R={:.3}",
            rgb[2 * plane + c],
            rgb[c]
        );
    }
}
