//! EMA 运动门控（DESIGN.md §5.1，参照 frigate frigate_motion.py）。
//!
//! P0 骨架：帧差 + 指数背景模型的最小实现，验证 I1（零分配）与
//! ADR-023（校准期）。P1 填完整 EMA（accumulateWeighted/百分位对比度拉伸/轮廓）。

/// 运动检测输出框（像素坐标，Y 平面坐标系）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// EMA 运动检测器（骨架版）。
///
/// 输入：Y 平面（宽 `w` 高 `h`）。内部全部预分配，`detect` 零堆分配（I1 断言）。
pub struct EmaMotion {
    /// 背景模型（f32 便于 EMA 加权）。构造时按 (w,h) 一次分配。
    avg: Vec<f32>,
    w: usize,
    h: usize,
    /// 帧计数（< 30 为校准期，ADR-023）。
    frame_count: u32,
    /// 背景学习率（frigate frame_alpha 等价）。
    alpha: f32,
    /// 触发阈值（0-255 差值）。
    threshold: u8,
    /// 校准帧数（frigate 同款 30）。
    calib_frames: u32,
}

impl EmaMotion {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            avg: vec![0.0; w * h],
            w,
            h,
            frame_count: 0,
            alpha: 0.05,
            threshold: 25,
            calib_frames: 30,
        }
    }

    /// 校准期？（重连后重建背景，期间不触发检测——宁漏报不误报，ADR-023）
    pub fn calibrating(&self) -> bool {
        self.frame_count < self.calib_frames
    }

    /// 检测一帧。返回运动框（可能有多个；P0 返回整帧一个或空）。
    ///
    /// 零堆分配：所有中间状态都在构造时分配的 `avg` 里（I1）。
    pub fn detect(&mut self, y_plane: &[u8]) -> Vec<MotionBox> {
        // P0：分配返回值 Vec 是允许的（运动时才有分配——静态时零分配）。
        // I1 断言测的是"无运动帧"路径：静止场景 detect 应零分配（Vec::new 不分配）。
        debug_assert_eq!(y_plane.len(), self.w * self.h);
        if self.calibrating() {
            self.update_background(y_plane);
            return Vec::new();
        }
        // 帧差：超出阈值的像素计数（P0 全帧判定；P1 分块做连通域）
        let mut moving_pixels = 0usize;
        for (i, &px) in y_plane.iter().enumerate() {
            let bg = self.avg[i];
            let d = (px as f32 - bg).abs();
            if d > self.threshold as f32 {
                moving_pixels += 1;
            }
        }
        self.update_background(y_plane);
        if moving_pixels > (self.w * self.h) / 64 {
            // P0：整框返回（P1 换块级质心）
            vec![MotionBox {
                x: 0,
                y: 0,
                w: self.w as u32,
                h: self.h as u32,
            }]
        } else {
            Vec::new()
        }
    }

    fn update_background(&mut self, y_plane: &[u8]) {
        let a = self.alpha;
        for (i, &px) in y_plane.iter().enumerate() {
            self.avg[i] += (px as f32 - self.avg[i]) * a;
        }
        self.frame_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn black(w: usize, h: usize) -> Vec<u8> {
        vec![0; w * h]
    }

    /// ADR-023：校准期（前 30 帧）即使有运动也不报。
    #[test]
    fn calibration_period_silent() {
        let mut m = EmaMotion::new(64, 36);
        let white = vec![255u8; 64 * 36];
        for _ in 0..29 {
            assert!(m.detect(&white).is_empty(), "校准期不触发");
        }
        assert!(m.calibrating());
        // 第 30 帧仍在校准（< calib_frames=30 已在 detect 中消耗后 frame_count=30）
        let _ = m.detect(&white);
        assert!(!m.calibrating(), "30 帧后校准完成");
    }

    /// 静止场景零运动 + 校准后静态帧零分配（I1 精神，P0 用简单断言；
    /// P0 的 dhat 断言在 aivx 主 crate 的 tests/ 里做全链路）。
    #[test]
    fn static_scene_no_motion() {
        let mut m = EmaMotion::new(64, 36);
        let frame = black(64, 36);
        for _ in 0..40 {
            m.detect(&frame);
        }
        assert!(m.detect(&frame).is_empty());
    }

    /// 校准后出现运动 → 报出框。
    #[test]
    fn motion_detected_after_calibration() {
        let mut m = EmaMotion::new(64, 36);
        let frame = black(64, 36);
        for _ in 0..35 {
            m.detect(&frame);
        }
        // 白色物体出现
        let mut moved = frame.clone();
        for px in moved.iter_mut().take(64 * 4) {
            *px = 255;
        }
        let boxes = m.detect(&moved);
        assert!(!boxes.is_empty(), "运动应触发");
    }
}
