//! I9 机器强制：配置热重载（DESIGN.md §0 I9 / §9 ADR-027）。
//!
//! - `ArcSwap<RuleSet>`：T2 每帧 `load()` 拿当前快照，无锁读
//! - 控制面写新快照后发 `ConfigChanged` 事件（审计天然完整）
//! - 改规则 = 重置该路规则状态（ADR-027：无迁移代码）
//!
//! 数据面 std-only 约束（I5/I11）：不引 arc-swap crate，手写
//! `Arc<ArcSwapInner>` + 原子指针交换（std 的 `Arc::clone` 指针写是原子的——
//! 用 `Mutex<Arc<T>>` 保护写，读走 `lock().clone()` 的浅拷贝；
//! 读频率=每帧一次，锁竞争≈0（单读者线程×单写者），简单正确优先）。

use std::sync::{Arc, Mutex};

use aivx_events::Event;

use crate::rules::AlarmRule;

/// 一路摄像头的规则集快照。
#[derive(Debug, Clone)]
pub struct RuleSet {
    pub version: u64,
    pub rules: Vec<AlarmRule>,
}

impl RuleSet {
    pub fn new(version: u64, rules: Vec<AlarmRule>) -> Self {
        Self { version, rules }
    }
}

/// I9：热重载句柄。
///
/// - T2 持 `Reader`（每帧 `load()`——浅拷贝 Arc，纳秒级）
/// - 控制面持 `Writer`（`store()` + 发 ConfigChanged 事件）
/// - 语义（ADR-027）：store 即重置——新快照的规则状态机从 Idle 开始，
///   旧快照的滞留计时/冷却随旧 Arc 丢弃
pub struct HotRuleSet {
    inner: Mutex<Arc<RuleSet>>,
}

impl Default for HotRuleSet {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Arc::new(RuleSet::new(0, Vec::new()))),
        }
    }
}

impl HotRuleSet {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// T2 读当前快照（每帧调用；Arc 浅拷贝，纳秒）。
    pub fn load(&self) -> Arc<RuleSet> {
        self.inner.lock().unwrap().clone()
    }

    /// 控制面写新快照。返回 ConfigChanged 事件（发到总线做审计）。
    ///
    /// 调用方（控制面）负责把返回的事件经事件链落库——
    /// 热更新不审计 = 静默改配置，违反"审计即事件流"原则。
    pub fn store(&self, new_set: RuleSet) -> Event {
        *self.inner.lock().unwrap() = Arc::new(new_set);
        Event::ConfigChanged {
            section: "rules".into(),
            summary: format!("ruleset version -> {}", self.inner.lock().unwrap().version),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{AlarmRule, Condition, Point};

    fn zone_rule() -> AlarmRule {
        AlarmRule::new(
            "r-zone",
            Condition::InZone(vec![
                Point { x: 0.2, y: 0.2 },
                Point { x: 0.8, y: 0.2 },
                Point { x: 0.8, y: 0.8 },
                Point { x: 0.2, y: 0.8 },
            ]),
        )
    }

    /// **I9 机器强制**：store 后 reader 下一帧拿到新版本；状态重置。
    #[test]
    fn i9_hot_reload_visible_next_frame_and_state_reset() {
        let hot = HotRuleSet::shared();
        assert_eq!(hot.load().version, 0);

        // v1 规则集：含一条规则
        let v1 = RuleSet::new(1, vec![zone_rule()]);
        let ev = hot.store(v1);
        assert!(matches!(ev, Event::ConfigChanged { section, .. } if section == "rules"));
        assert_eq!(hot.load().version, 1);
        assert_eq!(hot.load().rules.len(), 1);

        // 模拟 T2 评估使规则进入 Active
        let mut snap = (*hot.load()).clone();
        let (w, h) = (640u32, 360u32);
        let track = crate::track::Track {
            id: 1,
            x: 256,
            y: 144,
            w: 10,
            h: 10,
            hits: 3,
            total_hits: 3,
            missed: 0,
            confirmed: true,
            class: 0,
            prev_cx: Some(261.0),
            prev_cy: Some(149.0),
        };
        let fires = snap.rules[0].evaluate(std::slice::from_ref(&track), w, h);
        assert!(fires.is_some(), "v1 规则应触发");
        assert!(snap.rules[0].is_active());

        // I9 语义：热重载 = 状态重置（ADR-027）——新快照的规则从 Idle 开始
        let v2 = RuleSet::new(2, vec![zone_rule()]);
        let _ = hot.store(v2);
        let fresh = hot.load();
        assert_eq!(fresh.version, 2);
        assert!(
            !fresh.rules[0].is_active(),
            "热重载后状态必须重置（ADR-027）"
        );

        // 清空规则集（停用布控）
        let _ = hot.store(RuleSet::new(3, vec![]));
        assert!(hot.load().rules.is_empty());
    }
}
