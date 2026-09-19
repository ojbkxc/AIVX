//! T2 分析线程（DESIGN.md §3.2）。
//!
//! 每帧流程（热路径，全部零堆分配——I1）：
//!   slot 读最新帧 → EMA 运动（Y 平面，~2ms）→ 运动首帧触发推理 →
//!   事件 try_send（I12 分级）
//!
//! P1 阶段：推理以 trait 注入（桩实现驱动端到端延迟断言 I3；
//! ort YOLO + ByteTrack + 规则引擎在 P2 换真实现，本文件热路径结构不变）。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::bridge::PlaneBridge;
use crate::frame::LatestFrameSlot;
use crate::motion::EmaMotion;
use aivx_events::{DeviceId, Event};

/// 推理接口（P2 换 ort YOLO + DetectorPool 攒批；签名保持不变）。
pub trait FrameAnalyzer: Send {
    /// 输入 NV12 帧副本（scratch），输出检测框。
    fn detect(&mut self, nv12: &[u8]) -> Vec<Det>;
}

/// 检测结果（P2 扩展 label/score/keypoints；P0 桩只有框）。
#[derive(Debug, Clone, Copy)]
pub struct Det {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// 运动即报一个框的桩（驱动 I3 断言；真 YOLO 在 P2）。
pub struct MotionStubAnalyzer;

impl FrameAnalyzer for MotionStubAnalyzer {
    fn detect(&mut self, _nv12: &[u8]) -> Vec<Det> {
        vec![Det {
            x: 16,
            y: 9,
            w: 32,
            h: 18,
        }]
    }
}

/// T2 分析循环入口（专用 OS 线程）。
pub fn analysis_loop(
    device_id: DeviceId,
    slot: Arc<LatestFrameSlot>,
    bridge: Arc<PlaneBridge>,
    mut analyzer: impl FrameAnalyzer,
) {
    let mut motion = EmaMotion::new(slot.width(), slot.height());
    let mut scratch: Vec<u8> = vec![0; slot.frame_size()]; // 预分配（I1）
    let mut last_gen: u64 = 0;
    let mut calib_deadline_gen: u64 = 0; // 校准截止代（首帧时定）

    loop {
        if bridge.control.stop.load(Ordering::Relaxed) {
            return;
        }
        if bridge.control.pause_analysis.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let Some(fr) = slot.read_latest() else {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        };
        if fr.gen == last_gen {
            std::thread::sleep(Duration::from_millis(1));
            continue; // 无新帧
        }
        if last_gen == 0 {
            calib_deadline_gen = fr.gen + 30; // ADR-023：首帧起 30 帧校准
        }
        last_gen = fr.gen;

        // 运动检测：借 Y 平面（零拷贝）；撕裂竞争则跳过本帧
        let boxes = match slot.borrow_y(&fr, |y| motion.detect(y)) {
            Some(b) => b,
            None => continue,
        };
        if boxes.is_empty() {
            continue; // 静止——不推理（I3 省钱：95% 的帧到此为止）
        }

        // 运动首帧 → 立即推理（事件驱动，非轮询）
        let infer_start = crate::mono_ns();
        let ok = slot.copy_nv12_to(&fr, &mut scratch);
        if !ok {
            continue;
        }
        let dets = analyzer.detect(&scratch);
        bridge
            .metrics
            .infer_ns_total
            .fetch_add(crate::mono_ns() - infer_start, Ordering::Relaxed);
        bridge.metrics.inferences.fetch_add(1, Ordering::Relaxed);

        // P0 事件形态：桩推理命中 → 报警（P2 换规则状态机，I8 去重）
        if !dets.is_empty() {
            let now = crate::mono_ns();
            bridge.mark_alarm(now);
            bridge.emit(Event::AlarmRaised {
                alarm_id: format!("stub-{}-{}", fr.gen, now),
                device_id: device_id.clone(),
                rule_id: "stub-motion".into(),
                zone_id: None,
                track: None,
                frame_gen: fr.gen,
                mono_ns: now,
            });
        }
        bridge
            .metrics
            .analyze_frames
            .fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivx_events::Event;

    /// 单线程写 slot 的测试辅助（生产写者是独占线程；测试模拟它的行为）。
    fn write_frame(slot: &Arc<LatestFrameSlot>, val: u8) {
        unsafe {
            (*(Arc::as_ptr(slot) as *mut LatestFrameSlot)).write_val(val);
        }
    }

    /// **I3 合成流端到端断言**（DESIGN.md §6/§19）：
    /// 校准期过后，静止→运动→AlarmRaised ≤ 150ms（CI 上限，含抖动余量）。
    ///
    /// 校准期（ADR-023）单独断言：校准内注入运动帧不得报警（宁漏报不误报）。
    #[test]
    fn synthetic_motion_to_alarm_under_150ms() {
        let slot = Arc::new(LatestFrameSlot::new(64, 36));
        let (bridge, rx) = PlaneBridge::new(256, None);
        let bridge = Arc::new(bridge);

        let t = {
            let slot = slot.clone();
            let bridge = bridge.clone();
            std::thread::spawn(move || {
                analysis_loop("cam-synth".into(), slot, bridge, MotionStubAnalyzer)
            })
        };

        // ── 校准期断言（ADR-023）：前 30 帧注入运动，不得报警 ──
        for _ in 0..29 {
            write_frame(&slot, 255); // 满屏"运动"
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(20));
        let leaked = rx.try_recv();
        assert!(
            matches!(leaked, Err(_)),
            "校准期内不得报警（ADR-023），却收到 {:?}",
            leaked.map(|e| e.grade())
        );

        // ── 校准完成后：注白帧（与黑背景形成运动），计时 ──
        let t0 = crate::mono_ns();
        write_frame(&slot, 255);

        let mut got_alarm = false;
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(150) {
            if let Ok(ev) = rx.try_recv() {
                if matches!(ev, Event::AlarmRaised { .. }) {
                    got_alarm = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        bridge.control.stop.store(true, Ordering::Relaxed);
        let _ = t.join();

        let elapsed_ms = (crate::mono_ns() - t0) as f64 / 1e6;
        assert!(got_alarm, "150ms 内未收到报警（{elapsed_ms:.1}ms）");
        println!("I3 synthetic motion→alarm: {elapsed_ms:.2}ms (budget <100ms, CI cap 150ms)");
    }
}
