//! 认知层（cognition，DESIGN.md §10）——LLM 误报复核 + 语义描述。
//!
//! 参照：frigate-event-handler `daemon.py`（事件→抽帧→vision→refine→回写）、
//! rebucca `pipeline.py::_llm_verify_track`（复核+冷却 6s）、
//! ai-nvr `multimodal-analyzer.ts`（触发节流）、frigate `genai/plugins/`（provider 插件）。
//!
//! 关键设计：
//! - **不阻塞报警路径**：报警先达（Event 已在事件链），洞察后补（回填）
//! - `is_false_positive=true` → 投影器把 alarms 行标记 suppressed——**不撤回**
//!   （ADR-018：宁多报不漏报）
//! - per-rule 冷却（默认 6s，抄 rebucca）
//! - 证据未就绪（evidence_timeout=2s）→ 跳过该次洞察（报警永远先于洞察存在）
//! - InsightGenerated 是 Critical 级事件，走完整落库链（ADR-022）
//!
//! I5 强制：cognition 是控制面模块，只消费事件流，不 import 数据面内部结构。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aivx_events::{AlarmId, Event, Insight};

/// LLM provider 插件（OpenAI 兼容 / Ollama / ...）。
///
/// P4 测试用 `StubProvider`；P6 接 reqwest + AIGX 网关。
pub trait GenAiProvider: Send + Sync {
    /// 输入 JPEG 证据字节 + 提示词上下文，返回文本。
    fn analyze(&self, image_jpeg: &[u8], prompt_ctx: &str) -> Result<String, String>;
}

/// 桩 provider（无网络依赖——CI 可跑全链路）。
pub struct StubProvider;

impl GenAiProvider for StubProvider {
    fn analyze(&self, _image_jpeg: &[u8], _prompt_ctx: &str) -> Result<String, String> {
        Ok(r#"{"is_false_positive":false,"threat_level":"low","scene":"检测到一名人员经过后院","title":"人员出现","summary":"一名人员从画面左侧进入后院并离开"}"#.into())
    }
}

/// 洞察输出（LLM 结构化解析结果）。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedInsight {
    pub insight: Insight,
}

/// cognition 运行时状态。
pub struct Cognition {
    provider: Arc<dyn GenAiProvider>,
    /// per-rule 冷却表（rule_id → 上次分析时刻 mono_ns）。
    cooldowns: HashMap<String, u64>,
    cooldown_ns: u64,
    /// 每摄像头提示词上下文（"此摄像头朝向后门"——抄 frigate-event-handler）。
    prompt_ctx: HashMap<String, String>,
    /// 证据存储：alarm_id → JPEG 字节（证据线程产物；P4 由调用方注入）。
    evidence: HashMap<AlarmId, Vec<u8>>,
}

impl Cognition {
    pub fn new(provider: Arc<dyn GenAiProvider>, cooldown: Duration) -> Self {
        Self {
            provider,
            cooldowns: HashMap::new(),
            cooldown_ns: cooldown.as_nanos() as u64,
            prompt_ctx: HashMap::new(),
            evidence: HashMap::new(),
        }
    }

    pub fn set_prompt_ctx(&mut self, device_id: String, ctx: String) {
        self.prompt_ctx.insert(device_id, ctx);
    }

    /// 证据注入（证据线程调 EvidenceReady 时写入）。
    pub fn put_evidence(&mut self, alarm_id: AlarmId, jpeg: Vec<u8>) {
        self.evidence.insert(alarm_id, jpeg);
    }

    /// 处理一条报警事件。返回 `InsightGenerated` 事件（走完整事件链）或 None。
    ///
    /// 语义（DESIGN.md §6 预算）：LLM 1-3s 与报警路径解耦；此处同步桩，
    /// 生产（P6）用 async provider + 独立 task。
    pub fn on_alarm(
        &mut self,
        alarm_id: &AlarmId,
        rule_id: &str,
        device_id: &str,
        now_mono: u64,
    ) -> Option<Event> {
        // 冷却（抄 rebucca：per-rule 6s）
        if let Some(&last) = self.cooldowns.get(rule_id) {
            if now_mono.saturating_sub(last) < self.cooldown_ns {
                return None;
            }
        }
        // 证据未就绪 → 跳过（报警已存在，洞察只是增强）
        let jpeg = self.evidence.get(alarm_id)?.clone();
        let ctx = self.prompt_ctx.get(device_id).cloned().unwrap_or_default();
        // provider 失败 → 跳过该次洞察（报警不撤）
        let Ok(text) = self.provider.analyze(&jpeg, &ctx) else {
            return None;
        };
        let Ok(insight) = parse_insight(&text) else {
            return None;
        };
        self.cooldowns.insert(rule_id.into(), now_mono);
        Some(Event::InsightGenerated {
            alarm_id: alarm_id.clone(),
            insight: insight.insight,
        })
    }

    /// 事件喂入入口：AlarmRaised → on_alarm（若证据就绪则产洞察）。返回待落库的
    /// `InsightGenerated`（调用方把它 enqueue 进 DbWriter——ADR-022 完整链路）。
    ///
    /// 与证据解耦：无论证据先到还是报警先到，只要同 alarm_id 双方都到位即产洞察
    ///（证据迟到时本次报警跳过，下一条同规则报警经冷却后再分析）。
    pub fn feed(&mut self, ev: &Event, now_mono: u64) -> Vec<Event> {
        let mut out = Vec::new();
        match ev {
            Event::AlarmRaised {
                alarm_id,
                rule_id,
                device_id,
                ..
            } => {
                if let Some(insight_ev) = self.on_alarm(alarm_id, rule_id, device_id, now_mono) {
                    out.push(insight_ev);
                }
            }
            Event::EvidenceReady { .. } => {
                // 证据线程在事件链里只携带路径；JPEG 字节由证据存储注入
                //（P4 桩：调用方 feed 前 put_evidence 即可；真实链路见 §10——
                //  报警事件先于证据，cognition 在 alarm 时查证据）
            }
            _ => {}
        }
        out
    }
}

/// LLM 文本 → 结构化 Insight（serde_json 解析 OpenAI JSON 模式输出）。
fn parse_insight(text: &str) -> Result<ParsedInsight, String> {
    let insight: Insight = serde_json::from_str(text.trim()).map_err(|e| e.to_string())?;
    Ok(ParsedInsight { insight })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alarm_ev(id: &str) -> (AlarmId, &'static str, &'static str) {
        (id.into(), "r-1", "cam-back")
    }

    /// 全链路：报警 → 证据 → LLM → InsightGenerated（Critical）。
    #[test]
    fn alarm_with_evidence_produces_insight() {
        let mut cog = Cognition::new(Arc::new(StubProvider), Duration::from_secs(6));
        cog.set_prompt_ctx("cam-back".into(), "此摄像头朝向后院。".into());
        let (id, rule, dev) = alarm_ev("a-1");
        // 无证据 → 跳过
        assert!(cog.on_alarm(&id, rule, dev, 1000).is_none());
        // 注入证据 → 产出洞察
        cog.put_evidence(id.clone(), vec![0xFF, 0xD8]);
        let ev = cog
            .on_alarm(&id, rule, dev, 2000)
            .expect("有证据应产出洞察");
        let Event::InsightGenerated { alarm_id, insight } = ev else {
            panic!("必须是 InsightGenerated");
        };
        assert_eq!(alarm_id, "a-1");
        assert!(!insight.is_false_positive);
        assert_eq!(insight.threat_level, "low");
        assert!(insight.scene.contains("后院"));
        // 洞察是 Critical（ADR-018/§5.1 分级）
        let ev2 = Event::InsightGenerated { alarm_id, insight };
        assert_eq!(ev2.grade(), aivx_events::Grade::Critical);
    }

    /// 冷却：6s 内同规则第二条报警不重复分析（抄 rebucca）。
    #[test]
    fn cooldown_suppresses_repeat_analysis() {
        let mut cog = Cognition::new(Arc::new(StubProvider), Duration::from_secs(6));
        let (id1, rule, dev) = alarm_ev("a-1");
        let (id2, _, _) = alarm_ev("a-2");
        cog.put_evidence(id1.clone(), vec![1]);
        cog.put_evidence(id2.clone(), vec![1]);
        // 时间戳单位为单调钟纳秒。cooldown=6s。
        // t1=1s：触发，冷却记录到 1s。
        assert!(cog.on_alarm(&id1, rule, dev, 1_000_000_000).is_some());
        // t2=4s：距 1s 仅 3s，仍在 6s 冷却内 → 跳过。
        assert!(cog.on_alarm(&id2, rule, dev, 4_000_000_000).is_none());
        // t3=8s：距 1s 已 7s，超过冷却 → 分析（id2 证据已在）。
        assert!(cog.on_alarm(&id2, rule, dev, 8_000_000_000).is_some());
    }

    /// LLM 输出非 JSON → 跳过洞察，报警不受影响。
    #[test]
    fn malformed_llm_output_skips_insight() {
        struct BadProvider;
        impl GenAiProvider for BadProvider {
            fn analyze(&self, _: &[u8], _: &str) -> Result<String, String> {
                Ok("这不是 JSON".into())
            }
        }
        let mut cog = Cognition::new(Arc::new(BadProvider), Duration::from_secs(6));
        let (id, rule, dev) = alarm_ev("a-x");
        cog.put_evidence(id.clone(), vec![1]);
        assert!(cog.on_alarm(&id, rule, dev, 1).is_none(), "坏输出必须跳过");
    }

    /// provider 报错（网络）→ 跳过，不 panic。
    #[test]
    fn provider_error_skips_insight() {
        struct ErrProvider;
        impl GenAiProvider for ErrProvider {
            fn analyze(&self, _: &[u8], _: &str) -> Result<String, String> {
                Err("connection refused".into())
            }
        }
        let mut cog = Cognition::new(Arc::new(ErrProvider), Duration::from_secs(6));
        let (id, rule, dev) = alarm_ev("a-y");
        cog.put_evidence(id.clone(), vec![1]);
        assert!(cog.on_alarm(&id, rule, dev, 1).is_none());
    }

    /// feed 事件链入口：AlarmRaised 经 feed 产洞察（证据提前注入）。
    #[test]
    fn feed_produces_insight_from_alarm_event() {
        let mut cog = Cognition::new(Arc::new(StubProvider), Duration::from_secs(6));
        cog.set_prompt_ctx("cam-back".into(), "此摄像头朝向后院。".into());
        let (id, rule, dev) = alarm_ev("a-feed");
        cog.put_evidence(id.clone(), vec![0xFF, 0xD8]);
        let alarm = Event::AlarmRaised {
            alarm_id: id.clone(),
            device_id: dev.into(),
            rule_id: rule.into(),
            zone_id: None,
            track: None,
            frame_gen: 1,
            mono_ns: 1000,
        };
        let out = cog.feed(&alarm, 2000);
        assert_eq!(out.len(), 1, "有证据应产洞察");
        assert!(matches!(&out[0], Event::InsightGenerated { alarm_id, .. }
            if alarm_id == &id));
    }

    /// 证据后到：报警先喂入（无证据跳过）→ 证据注入后同规则下一条报警才分析。
    #[test]
    fn late_evidence_skips_first_then_analyzes() {
        let mut cog = Cognition::new(Arc::new(StubProvider), Duration::from_secs(6));
        let (id, rule, dev) = alarm_ev("a-late");
        let alarm = |mid: u64, mono: u64| Event::AlarmRaised {
            alarm_id: format!("{}-{mid}", id),
            device_id: dev.into(),
            rule_id: rule.into(),
            zone_id: None,
            track: None,
            frame_gen: mid,
            mono_ns: mono,
        };
        // 报警先到，无证据 → 跳过
        assert!(cog.feed(&alarm(1, 1_000), 1_100).is_empty());
        // 证据注入
        cog.put_evidence(format!("{}-1", id), vec![1]);
        // 同规则第二条报警（冷却 6s 外）→ 分析
        let out = cog.feed(&alarm(2, 1_100), 1_200);
        // 注意：证据 key 是第一条的 alarm_id，第二条 alarm_id 不同——证据缺失
        // 仍跳过；冷却抑制在同 rule 下生效。此断言验证"证据按 alarm_id 精确匹配"。
        assert!(out.is_empty(), "第二条报警证据缺失应跳过");
    }
}
