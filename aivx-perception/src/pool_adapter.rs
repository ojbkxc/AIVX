//! DetectorPool → FrameAnalyzer 适配器（DESIGN.md §4 / P8）。
//!
//! 把 [`DetectorPool`]（攒批推理池）桥接为 [`FrameAnalyzer`]（分析循环接口）——
//! 这就是"OrtYoloBackend 接入分析循环"的端到端路径：
//!   analyze.rs T2 → PoolAnalyzer::detect → DetectorPool::infer（攒批）
//!   → worker 后端（OrtYoloBackend）→ 结果槽写回 → 返回 dets → 报警。
//!
//! 生产用法：把 `PoolAnalyzer` 注入 `analysis_loop`，内部池用 `OrtYoloBackend`。
//! 测试用 `SyncStubBackend` 驱动端到端（无模型也能验证链路）。

use crate::analyze::{Det, FrameAnalyzer};
use crate::pool::{DetectorPool, EngineKey, InferBackend};

/// DetectorPool 的 FrameAnalyzer 适配器。
pub struct PoolAnalyzer {
    pool: std::sync::Arc<DetectorPool>,
    key: EngineKey,
}

impl PoolAnalyzer {
    /// 构造：池 + 引擎键（模型/设备/输入尺寸）。
    pub fn new(pool: std::sync::Arc<DetectorPool>, key: EngineKey) -> Self {
        Self { pool, key }
    }

    /// 从池构造（内部起 worker + 默认键）。
    pub fn with_backend(backend: impl InferBackend + 'static, key: EngineKey) -> Self {
        let pool = std::sync::Arc::new(DetectorPool::new(backend));
        let pool_worker = pool.clone();
        std::thread::spawn(move || pool_worker.run_worker());
        std::thread::sleep(std::time::Duration::from_millis(10));
        Self::new(pool, key)
    }
}

impl FrameAnalyzer for PoolAnalyzer {
    fn detect(&mut self, nv12: &[u8]) -> Vec<Det> {
        // 提交推理 → 阻塞等 worker 写回真实结果（攒批路径）
        self.pool.infer(&self.key, nv12.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::SyncStubBackend;

    /// 端到端：PoolAnalyzer → DetectorPool → worker → 真实结果返回。
    /// 这就是"推理接入分析循环"的机器验证（无模型，用 SyncStubBackend）。
    #[test]
    fn pool_analyzer_returns_dets_end_to_end() {
        let key = EngineKey {
            model_id: "yolov8n".into(),
            device: "cpu".into(),
            input_w: 640,
            input_h: 640,
        };
        let mut analyzer = PoolAnalyzer::with_backend(SyncStubBackend, key);
        let dets = analyzer.detect(&vec![128u8; 640 * 360]);
        assert_eq!(dets.len(), 1, "适配器应拿到真实结果");
        assert_eq!(
            dets[0],
            Det {
                x: 16,
                y: 9,
                w: 32,
                h: 18
            }
        );
    }

    /// 并发多路：多个 PoolAnalyzer 共享同一池（跨路攒批）。
    #[test]
    fn multiple_analyzers_share_pool() {
        let pool = std::sync::Arc::new(DetectorPool::new(SyncStubBackend));
        let pool_worker = pool.clone();
        std::thread::spawn(move || pool_worker.run_worker());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let key = EngineKey {
            model_id: "m".into(),
            device: "cpu".into(),
            input_w: 64,
            input_h: 36,
        };
        let mut a1 = PoolAnalyzer::new(pool.clone(), key.clone());
        let mut a2 = PoolAnalyzer::new(pool.clone(), key);
        let d1 = a1.detect(&vec![0; 64 * 36]);
        let d2 = a2.detect(&vec![0; 64 * 36]);
        assert_eq!(d1.len(), 1);
        assert_eq!(d2.len(), 1, "两路共享池都应拿到结果");
    }
}
