//! ByteTrack 式多目标跟踪（DESIGN.md §5.3）。
//!
//! 参照：ai-nvr（ByteTrack，min_hits=3 防幽灵 ID）+ rebucca `tracker.py`
//! （IoU 贪心匹配，max_missed 后判定消失）。Rust 版合一：
//! - IoU 贪心匹配（同 label 间）
//! - `min_hits`：新轨迹需连续 N 帧命中才**确认**（进 active 输出）——短暂误检
//!   不产生幽灵目标（抄 ai-nvr）
//! - `max_missed`：连续 N 帧失配则轨迹结束
//! - 输出 (active, ended)：规则状态机消费 active；TrackDisappeared 事件消费 ended

use std::collections::HashMap;

use crate::analyze::Det;

/// 轨迹 ID（数据面内单调分配；跨重启不持久——轨迹是会话级概念）。
pub type TrackId = u64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub hits: u32,
    /// 连续命中次数（含未确认期）。
    pub total_hits: u32,
    pub missed: u32,
    pub confirmed: bool,
    /// 上一帧中心（越线/方向判定用；首帧 None）。
    pub prev_cx: Option<f32>,
    pub prev_cy: Option<f32>,
}

/// IoU（轴对齐框）。
fn iou(a: &Det, b: &Track) -> f32 {
    let ax2 = a.x + a.w;
    let ay2 = a.y + a.h;
    let bx2 = b.x + b.w;
    let by2 = b.y + b.h;
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = ix2.saturating_sub(ix1);
    let ih = iy2.saturating_sub(iy1);
    let inter = (iw * ih) as f32;
    let ua = (a.w * a.h) as f32;
    let ub = (b.w * b.h) as f32;
    if ua + ub <= 0.0 {
        return 0.0;
    }
    inter / (ua + ub - inter)
}

/// 跟踪器。每路一个（T2 独占——无锁，I2）。
pub struct ByteTracker {
    iou_threshold: f32,
    min_hits: u32,
    max_missed: u32,
    tracks: Vec<Track>,
    next_id: TrackId,
    /// 本帧结束的轨迹 id（ended 输出）。
    ended_buf: Vec<TrackId>,
    /// 新确认的轨迹（appeared 事件用）。
    appeared_buf: Vec<Track>,
}

impl ByteTracker {
    pub fn new() -> Self {
        Self {
            iou_threshold: 0.3,
            min_hits: 3,
            max_missed: 8,
            tracks: Vec::with_capacity(32),
            next_id: 1,
            ended_buf: Vec::with_capacity(8),
            appeared_buf: Vec::with_capacity(8),
        }
    }

    /// 喂入一帧检测。返回当前确认活跃轨迹（含位置更新）。
    ///
    /// `ended`/`appeared` 经 `take_ended`/`take_appeared` 取走（零拷贝转移）。
    pub fn update(&mut self, dets: &[Det]) -> &[Track] {
        self.ended_buf.clear();
        self.appeared_buf.clear();
        // 贪心匹配：每个检测找 IoU 最大且超阈值的轨迹
        let mut det_matched = vec![false; dets.len()];
        let mut tr_matched = vec![false; self.tracks.len()];
        for (di, det) in dets.iter().enumerate() {
            if det_matched[di] {
                continue;
            }
            let mut best: Option<usize> = None;
            let mut best_iou = self.iou_threshold;
            for (ti, tr) in self.tracks.iter().enumerate() {
                if tr_matched[ti] {
                    continue;
                }
                let v = iou(det, tr);
                if v > best_iou {
                    best_iou = v;
                    best = Some(ti);
                }
            }
            if let Some(ti) = best {
                det_matched[di] = true;
                tr_matched[ti] = true;
                let tr = &mut self.tracks[ti];
                let cx = det.x as f32 + det.w as f32 / 2.0;
                let cy = det.y as f32 + det.h as f32 / 2.0;
                tr.prev_cx = Some(cx);
                tr.prev_cy = Some(cy);
                tr.x = det.x;
                tr.y = det.y;
                tr.w = det.w;
                tr.h = det.h;
                tr.missed = 0;
                tr.total_hits += 1;
                if !tr.confirmed {
                    tr.hits += 1;
                    if tr.hits >= self.min_hits {
                        tr.confirmed = true;
                        self.appeared_buf.push(*tr);
                    }
                }
            }
        }
        // 未匹配轨迹：missed 累计；超限结束
        let mut i = 0;
        while i < self.tracks.len() {
            if tr_matched[i] {
                i += 1;
                continue;
            }
            let tr = &mut self.tracks[i];
            tr.missed += 1;
            if tr.missed >= self.max_missed {
                let id = tr.id;
                self.ended_buf.push(id);
                self.tracks.swap_remove(i); // O(1) 删除；顺序无所谓（id 唯一）
            } else {
                i += 1;
            }
        }
        // 未匹配检测：新轨迹（未确认）
        for (di, det) in dets.iter().enumerate() {
            if det_matched[di] {
                continue;
            }
            self.tracks.push(Track {
                id: self.next_id,
                x: det.x,
                y: det.y,
                w: det.w,
                h: det.h,
                hits: 1,
                total_hits: 1,
                missed: 0,
                confirmed: false,
                prev_cx: Some(det.x as f32 + det.w as f32 / 2.0),
                prev_cy: Some(det.y as f32 + det.h as f32 / 2.0),
            });
            // min_hits=1 时直接确认
            if self.min_hits <= 1 {
                let t = self.tracks.last_mut().unwrap();
                t.confirmed = true;
                self.appeared_buf.push(*t);
            }
            self.next_id += 1;
        }
        &self.tracks
    }

    /// 确认活跃轨迹（供规则引擎消费）。
    pub fn active(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.confirmed)
    }

    /// 取走本帧结束的轨迹 id。
    pub fn take_ended(&mut self) -> Vec<TrackId> {
        std::mem::take(&mut self.ended_buf)
    }

    /// 取走本帧新确认的轨迹。
    pub fn take_appeared(&mut self) -> Vec<Track> {
        std::mem::take(&mut self.appeared_buf)
    }
}

impl Default for ByteTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(x: u32, y: u32) -> Det {
        Det { x, y, w: 10, h: 10 }
    }

    /// min_hits=3：单帧误检不确认（防幽灵 ID，抄 ai-nvr ByteTrack 语义）。
    #[test]
    fn single_frame_noise_not_confirmed() {
        let mut t = ByteTracker::new();
        t.update(&[det(0, 0)]); // 一帧后消失
        t.update(&[]);
        assert_eq!(t.active().count(), 0, "单帧检测不得成为确认目标");
    }

    /// 连续 3 帧命中 → 确认 → appeared 恰一次。
    #[test]
    fn three_hits_confirm() {
        let mut t = ByteTracker::new();
        t.update(&[det(0, 0)]);
        assert_eq!(t.active().count(), 0);
        t.update(&[det(1, 0)]);
        assert_eq!(t.active().count(), 0);
        t.update(&[det(2, 0)]);
        let appeared = t.take_appeared();
        assert_eq!(appeared.len(), 1);
        assert_eq!(appeared[0].id, 1);
        assert_eq!(t.active().count(), 1);
    }

    /// 同一目标移动：IoU 匹配保持同 id；位置更新。
    #[test]
    fn same_target_keeps_id() {
        let mut t = ByteTracker::new();
        for i in 0..5u32 {
            t.update(&[det(i * 2, 0)]); // 每帧右移 2px，IoU 高
        }
        let act: Vec<&Track> = t.active().collect();
        assert_eq!(act.len(), 1);
        assert_eq!(act[0].id, 1);
        assert_eq!(act[0].x, 8);
    }

    /// 失配 8 帧 → 轨迹结束 → ended 输出。
    #[test]
    fn missed_ends_track() {
        let mut t = ByteTracker::new();
        for _ in 0..3 {
            t.update(&[det(0, 0)]);
        }
        assert_eq!(t.active().count(), 1);
        for _ in 0..8 {
            t.update(&[]);
        }
        let ended = t.take_ended();
        assert_eq!(ended, vec![1]);
        assert_eq!(t.active().count(), 0);
    }

    /// 两个目标交叉：各自保持 id（IoU 贪心）。
    #[test]
    fn two_targets_tracked() {
        let mut t = ByteTracker::new();
        for i in 0..6u32 {
            t.update(&[det(i * 2, 0), det(100, 100)]);
        }
        let act: Vec<&Track> = t.active().collect();
        assert_eq!(act.len(), 2);
        let ids: Vec<TrackId> = act.iter().map(|a| a.id).collect();
        assert!(ids.contains(&1) && ids.contains(&2));
    }
}
