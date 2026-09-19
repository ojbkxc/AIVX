//! 布控规则引擎（DESIGN.md §9）——真表达式树 + 状态机（I8）。
//!
//! 参照：ai-nvr `alert/engine.rs`（条件树+滑动窗口）+ rebucca `biz_rules.py`
//! （几何：点在多边形/叉积越线/角度窗）。
//!
//! I8 的机器强制：`AlarmRule` 内部状态机保证**同一目标同一规则只在
//! Idle→Active 跳变时发 AlarmRaised**——连续帧同目标只报一次。

use crate::track::Track;

/// 归一化坐标点（0.0..=1.0，前端布控绘制即存此格式）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// 几何：点在多边形内（射线法——抄 rebucca biz_rules._point_in_polygon）。
pub fn point_in_polygon(px: f32, py: f32, poly: &[Point]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = (poly[i].x, poly[i].y);
        let (xj, yj) = (poly[j].x, poly[j].y);
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// 几何：轨迹段 prev→cur 是否跨过有向线 a→b（叉积判向）。
/// 返回 Some(true)=正向（左侧→右侧），Some(false)=逆向，None=未跨线。
/// 抄 rebucca biz_rules.cross_line_direction。
pub fn cross_line_direction(
    prev: (f32, f32),
    cur: (f32, f32),
    a: (f32, f32),
    b: (f32, f32),
) -> Option<bool> {
    let cross = |o: (f32, f32), p: (f32, f32), q: (f32, f32)| -> f32 {
        (p.0 - o.0) * (q.1 - o.1) - (p.1 - o.1) * (q.0 - o.0)
    };
    let c1 = cross(a, b, prev);
    let c2 = cross(a, b, cur);
    if c1 == 0.0 || c2 == 0.0 || c1 * c2 > 0.0 {
        return None; // 同侧
    }
    Some(c1 > 0.0 && c2 < 0.0) // 正向：左→右
}

/// 条件树（DESIGN.md §9：And/Or/Not 递归）。
#[derive(Debug, Clone)]
pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    /// 目标中心在区域内（zone 多边形，归一化坐标）。
    InZone(Vec<Point>),
    /// 跨越有向线（a→b），direction=true 正向。
    CrossedLine {
        a: Point,
        b: Point,
        forward: bool,
    },
    /// 区域内目标数 > n。
    ZoneCountOver {
        poly: Vec<Point>,
        n: u32,
    },
}

impl Condition {
    /// 对"当前帧状态"求值。
    ///
    /// `tracks`：确认活跃轨迹；`frame_w/h`：子码流分辨率（归一化→像素换算）。
    pub fn eval(&self, tracks: &[Track], frame_w: u32, frame_h: u32) -> bool {
        match self {
            Condition::All(cs) => cs.iter().all(|c| c.eval(tracks, frame_w, frame_h)),
            Condition::Any(cs) => cs.iter().any(|c| c.eval(tracks, frame_w, frame_h)),
            Condition::Not(c) => !c.eval(tracks, frame_w, frame_h),
            Condition::InZone(poly) => tracks.iter().any(|t| {
                let cx = (t.x + t.w / 2) as f32 / frame_w as f32;
                let cy = (t.y + t.h / 2) as f32 / frame_h as f32;
                point_in_polygon(cx, cy, poly)
            }),
            Condition::CrossedLine { a, b, forward } => tracks.iter().any(|t| {
                let (Some(pcx), Some(pcy)) = (t.prev_cx, t.prev_cy) else {
                    return false;
                };
                cross_line_direction(
                    (pcx / frame_w as f32, pcy / frame_h as f32),
                    (
                        (t.x + t.w / 2) as f32 / frame_w as f32,
                        (t.y + t.h / 2) as f32 / frame_h as f32,
                    ),
                    (a.x, a.y),
                    (b.x, b.y),
                )
                .map(|fwd| fwd == *forward)
                .unwrap_or(false)
            }),
            Condition::ZoneCountOver { poly, n } => {
                let count = tracks
                    .iter()
                    .filter(|t| {
                        let cx = (t.x + t.w / 2) as f32 / frame_w as f32;
                        let cy = (t.y + t.h / 2) as f32 / frame_h as f32;
                        point_in_polygon(cx, cy, poly)
                    })
                    .count() as u32;
                count > *n
            }
        }
    }
}

/// 规则触发输出（T2 组装事件用）。
#[derive(Debug, Clone, PartialEq)]
pub struct RuleFire {
    pub rule_id: String,
    pub track_id: Option<u64>,
}

/// 单条规则的运行时状态机（I8 核心）。
///
/// 状态：Idle →（条件真）→ Active（发 AlarmRaised 一次）
///       →（条件假）→ Idle（发 AlarmCleared 一次）
/// 连续帧条件保持真：**不重复发**。
#[derive(Debug, Clone)]
pub struct AlarmRule {
    pub rule_id: String,
    pub condition: Condition,
    active: bool,
}

impl AlarmRule {
    pub fn new(rule_id: impl Into<String>, condition: Condition) -> Self {
        Self {
            rule_id: rule_id.into(),
            condition,
            active: false,
        }
    }

    /// 喂入一帧，输出跳变（I8：只有状态迁移才产生输出）。
    pub fn evaluate(&mut self, tracks: &[Track], frame_w: u32, frame_h: u32) -> Option<RuleFire> {
        let cond = self.condition.eval(tracks, frame_w, frame_h);
        match (self.active, cond) {
            (false, true) => {
                self.active = true;
                Some(RuleFire {
                    rule_id: self.rule_id.clone(),
                    track_id: tracks.first().map(|t| t.id),
                })
            }
            (true, false) => {
                self.active = false;
                None // P0：Cleared 由事件层在规则引擎统一处理；此处只报 Raised
            }
            _ => None, // 状态未变——不重复（I8）
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

/// 规则引擎（T2 独占；P1 的规则快照热替换走 ArcSwap，P2 简化为直接持有）。
pub struct RuleEngine {
    rules: Vec<AlarmRule>,
}

impl RuleEngine {
    pub fn new(rules: Vec<AlarmRule>) -> Self {
        Self { rules }
    }

    pub fn force_detect(&self) -> bool {
        // 有任意规则存在即持续检测（P0 简化；P1 按 Condition 类型细分）
        !self.rules.is_empty()
    }

    /// 评估一帧。返回触发的规则（可能多条）。
    pub fn evaluate(&mut self, tracks: &[Track], frame_w: u32, frame_h: u32) -> Vec<RuleFire> {
        self.rules
            .iter_mut()
            .filter_map(|r| r.evaluate(tracks, frame_w, frame_h))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq_track(id: u64, x: u32, y: u32) -> Track {
        Track {
            id,
            x,
            y,
            w: 10,
            h: 10,
            hits: 3,
            total_hits: 3,
            missed: 0,
            confirmed: true,
            prev_cx: Some(x as f32 + 5.0),
            prev_cy: Some(y as f32 + 5.0),
        }
    }

    fn zone() -> Vec<Point> {
        vec![
            Point { x: 0.2, y: 0.2 },
            Point { x: 0.8, y: 0.2 },
            Point { x: 0.8, y: 0.8 },
            Point { x: 0.2, y: 0.8 },
        ]
    }

    /// 几何正确性：点在多边形（抄 rebucca biz_rules 用例结构）。
    #[test]
    fn polygon_geometry() {
        let z = zone();
        assert!(point_in_polygon(0.5, 0.5, &z));
        assert!(!point_in_polygon(0.1, 0.5, &z));
        assert!(!point_in_polygon(0.95, 0.95, &z));
    }

    /// 几何正确性：叉积越线方向（语义对齐 rebucca `biz_rules.cross_line_direction`）。
    ///
    /// 线 a=(0.3,0.5)→b=(0.7,0.5) 朝右。cross(a,b,p) = 0.4×(p.y−0.5)：
    /// 点在线上方(y<0.5) 为负，下方为正。rebucca 语义 forward = c1>0>c2
    /// （**下方→上方**）；reverse = c1<0<c2（上方→下方）。
    #[test]
    fn cross_line_direction_geometry() {
        let a = (0.3, 0.5);
        let b = (0.7, 0.5);
        // 上(0.5,0.3)→下(0.5,0.7)：c1<0<c2 → reverse
        assert_eq!(
            cross_line_direction((0.5, 0.3), (0.5, 0.7), a, b),
            Some(false)
        );
        // 下→上：c1>0>c2 → forward
        assert_eq!(
            cross_line_direction((0.5, 0.7), (0.5, 0.3), a, b),
            Some(true)
        );
        // 同侧不跨
        assert_eq!(cross_line_direction((0.1, 0.3), (0.15, 0.3), a, b), None);
    }

    /// **I8 机器强制**：连续帧同目标在区域内——只发一次 AlarmRaised。
    #[test]
    fn i8_dedup_only_fires_on_transition() {
        let mut eng = RuleEngine::new(vec![AlarmRule::new("r-zone", Condition::InZone(zone()))]);
        let (w, h) = (640u32, 360u32);
        // 中心 (261,149)/640,360=(0.41,0.41) 在 zone 0.2..0.8 内
        let t = vec![sq_track(1, 256, 144)];
        let fires1 = eng.evaluate(&t, w, h);
        assert_eq!(fires1.len(), 1, "首帧命中应触发");
        // 连续 50 帧同目标——不得重复
        for _ in 0..50 {
            let fires = eng.evaluate(&t, w, h);
            assert!(fires.is_empty(), "I8 违反：同目标持续在场重复报警");
        }
        // 目标离开 → 状态复位
        let gone = vec![sq_track(1, 640, 355)]; // 出 zone
        assert!(eng.evaluate(&gone, w, h).is_empty());
        // 再次进入 → 再触发（新的一次跳变）
        let back = vec![sq_track(1, 256, 144)];
        let fires2 = eng.evaluate(&back, w, h);
        assert_eq!(fires2.len(), 1, "离开后再进入应重新触发");
    }

    /// 条件树：All/Any/Not 组合语义。
    #[test]
    fn condition_tree_combinators() {
        let z = zone();
        let in_zone = Condition::InZone(z.clone());
        let out_zone = Condition::Not(Box::new(Condition::InZone(z.clone())));
        let (w, h) = (640u32, 360u32);
        let inside = vec![sq_track(1, 256, 144)];
        let outside = vec![sq_track(1, 630, 350)];

        assert!(Condition::Any(vec![in_zone.clone(), out_zone.clone()]).eval(&inside, w, h));
        assert!(!Condition::All(vec![in_zone.clone(), out_zone.clone()]).eval(&inside, w, h));
        assert!(Condition::Not(Box::new(in_zone.clone())).eval(&outside, w, h));
        assert!(out_zone.eval(&outside, w, h));
    }

    /// 越线规则：目标跨线触发一次（forward = rebucca 语义：c1>0>c2，即
    /// 线下方→线上方；见 cross_line_direction_geometry 的数学推导）。
    #[test]
    fn crossed_line_fires_once() {
        let mut eng = RuleEngine::new(vec![AlarmRule::new(
            "r-line",
            Condition::CrossedLine {
                a: Point { x: 0.3, y: 0.5 },
                b: Point { x: 0.7, y: 0.5 },
                forward: true, // 下方→上方
            },
        )]);
        let (w, h) = (640u32, 360u32);
        // 帧1：在线下方 中心(320,216)=(0.5,0.6)——匹配(prev 有值)但 prev==cur 不跨
        let below = vec![sq_track(1, 315, 211)];
        assert!(eng.evaluate(&below, w, h).is_empty());
        // 帧2：prev=(0.5,0.6)→cur=(0.5,0.4) 下→上跨线 forward → 触发一次
        let mut above = sq_track(1, 315, 139); // 中心(320,144)=(0.5,0.4)
        above.prev_cy = Some(216.0);
        let fires = eng.evaluate(&[above], w, h);
        assert_eq!(fires.len(), 1, "下→上 forward 跨线应触发");
        // 帧3：继续在上方——不重复（I8）
        let above2 = vec![sq_track(1, 315, 139)];
        assert!(eng.evaluate(&above2, w, h).is_empty());
    }
}
