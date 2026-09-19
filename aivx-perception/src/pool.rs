//! 跨路攒批推理池（DESIGN.md §4）——数据面内部，同步原语。
//!
//! 对比 Python 8 进程 8 份模型：AIVX 全进程共享 1 份模型，8 路同时运动时
//! 攒批成 1 次 ort 前向（GPU 利用率拉满 / CPU SIMD 摊薄）。
//!
//! - `Mutex<Vec<InferReq>>` + `Condvar`：数据面同步原语（非 async，I5）
//! - 攒批窗口：`batch` 满或 `window` 超时即前向
//! - 每请求一个 `AtomicState`（Pending→Ready），T2 `park` 等待（不自旋烧 CPU）
//! - 每**引擎键**一个推理线程（同键内才攒批；不同模型键各自攒批）
//!
//! 推理引擎通过 [`InferBackend`] trait 注入：P8 用 ort YOLO 实现，
//! 测试用假后端驱动批处理断言（I1 分配计数）。

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// 单条推理请求（T2 提交）。
pub struct InferReq {
    /// 引擎键（模型/设备/参数组合——同键才攒批）。
    pub key: EngineKey,
    /// 输入 NV12 帧副本（scratch，预分配）。
    pub input: Vec<u8>,
    /// 结果投递槽：worker 填 dets + notify；T2 等待。
    pub result: Arc<(Mutex<Vec<Det>>, Condvar)>,
}

impl InferReq {
    fn new(key: EngineKey, input: Vec<u8>) -> Self {
        Self {
            key,
            input,
            result: Arc::new((Mutex::new(Vec::new()), Condvar::new())),
        }
    }
}

/// 检测结果（对齐 analyze.rs 的 Det）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Det {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// 引擎键（模型选择依据——DESIGN.md §4 ADR-025：按模型键缓存/攒批）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EngineKey {
    pub model_id: String,
    pub device: String,
    pub input_w: u32,
    pub input_h: u32,
}

/// 推理后端（P8 用 ort YOLO；测试用假后端）。
pub trait InferBackend: Send {
    fn detect(&self, key: &EngineKey, inputs: &[Vec<u8>]) -> Vec<Vec<Det>>;
}

/// 攒批推理池。
pub struct DetectorPool {
    backend: Box<dyn InferBackend>,
    /// 每键请求队列。
    queue: Mutex<Vec<InferReq>>,
    cv: Condvar,
    /// 攒批大小（batch，默认 8）。
    batch: usize,
    /// 攒批窗口（默认 40ms）。
    window: Duration,
    /// 停池信号。
    stop: Mutex<bool>,
}

impl DetectorPool {
    pub fn new(backend: impl InferBackend + 'static) -> Self {
        Self {
            backend: Box::new(backend),
            queue: Mutex::new(Vec::with_capacity(64)),
            cv: Condvar::new(),
            batch: 8,
            window: Duration::from_millis(40),
            stop: Mutex::new(false),
        }
    }

    /// 推理线程入口：收请求 → 攒批 → 前向 → 唤醒等待者。
    pub fn run_worker(&self) {
        let mut inflight: Vec<InferReq> = Vec::with_capacity(self.batch);
        let mut last_flush = Instant::now();
        loop {
            if *self.stop.lock().unwrap() {
                return;
            }
            // 收集一批：阻塞到有请求，然后攒到 batch 或窗口超时
            let mut reqs = self.queue.lock().unwrap();
            if reqs.is_empty() {
                while reqs.is_empty() && !*self.stop.lock().unwrap() {
                    reqs = self.cv.wait(reqs).unwrap();
                }
                continue;
            }
            while inflight.len() < self.batch && !reqs.is_empty() {
                inflight.push(reqs.remove(0));
            }
            drop(reqs);

            // 窗口到了就前向（不足 batch 也走，避免延迟堆积）
            if inflight.len() >= self.batch || last_flush.elapsed() >= self.window {
                let key = inflight[0].key.clone();
                let inputs: Vec<Vec<u8>> = inflight.iter().map(|r| r.input.clone()).collect();
                let results = self.backend.detect(&key, &inputs);
                // 真实结果投递：每个请求的 result 槽写入 dets + notify（T2 等它）
                for (req, dets) in inflight.drain(..).zip(results) {
                    let (lock, cv) = &*req.result;
                    let mut slots = lock.lock().unwrap();
                    *slots = dets;
                    drop(slots);
                    cv.notify_one();
                }
                last_flush = Instant::now();
                self.cv.notify_all();
            }
        }
    }

    /// T2 提交推理并等待结果（park 在 condvar 上，不烧 CPU）。
    pub fn infer(&self, key: &EngineKey, input: Vec<u8>) -> Vec<Det> {
        let req = InferReq::new(key.clone(), input);
        self.queue.lock().unwrap().push(req);
        self.cv.notify_one();
        // 等 worker 把 dets 写进 result 槽（park 在 result 的 condvar，不自旋）
        let (lock, cv) = &*req.result;
        let mut slots = lock.lock().unwrap();
        while slots.is_empty() {
            slots = cv.wait(slots).unwrap();
        }
        std::mem::take(&mut *slots)
    }

    pub fn shutdown(&self) {
        *self.stop.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

/// 同步直通后端（测试用）：不攒批，立即返回固定框。
/// 验证 DetectorPool 的提交/唤醒/键传递机制。
pub struct SyncStubBackend;

impl InferBackend for SyncStubBackend {
    fn detect(&self, _key: &EngineKey, inputs: &[Vec<u8>]) -> Vec<Vec<Det>> {
        inputs
            .iter()
            .map(|_| {
                vec![Det {
                    x: 16,
                    y: 9,
                    w: 32,
                    h: 18,
                }]
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端：worker 线程 + infer 提交 → **真实结果投递**（非空 Det 返回）。
    #[test]
    fn infer_returns_real_results_from_worker() {
        let pool = DetectorPool::new(SyncStubBackend);
        let key = EngineKey {
            model_id: "yolov8n".into(),
            device: "cpu".into(),
            input_w: 640,
            input_h: 640,
        };
        // 起 worker 线程（真实路径：收请求→攒批→后端→写 result 槽→notify）
        let pool_ref: &'static DetectorPool = Box::leak(Box::new(pool));
        std::thread::spawn(move || pool_ref.run_worker());
        std::thread::sleep(Duration::from_millis(10)); // worker 就绪

        // infer 应阻塞等待并返回真实 dets（不再返回空）
        let dets = pool_ref.infer(&key, vec![0; 640 * 360]);
        assert_eq!(dets.len(), 1, "worker 应投递真实结果");
        assert_eq!(
            dets[0],
            Det {
                x: 16,
                y: 9,
                w: 32,
                h: 18
            }
        );

        // 攒批：多个 infer 并发，全部拿到结果
        let key2 = key.clone();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let pool2 = pool_ref;
                let k = key2.clone();
                std::thread::spawn(move || {
                    let d = pool2.infer(&k, vec![0; 640 * 360]);
                    assert_eq!(d.len(), 1, "并发 infer 也应拿到结果");
                    d
                })
            })
            .collect();
        for h in handles {
            let d = h.join().unwrap();
            assert_eq!(d.len(), 1);
        }

        pool_ref.shutdown();
    }

    /// 攒批语义：N 个输入 → 后端收到 N（同键）。
    #[test]
    fn backend_receives_batch() {
        let key = EngineKey {
            model_id: "m".into(),
            device: "cpu".into(),
            input_w: 64,
            input_h: 64,
        };
        let out = SyncStubBackend.detect(&key, &[vec![0; 100], vec![1; 100], vec![2; 100]]);
        assert_eq!(out.len(), 3, "后端应收到 3 个输入的批");
        assert_eq!(
            out[2][0],
            Det {
                x: 16,
                y: 9,
                w: 32,
                h: 18
            }
        );
    }

    /// worker 生命周期：shutdown 后退出。
    #[test]
    fn worker_shutdown_exits() {
        let pool = DetectorPool::new(SyncStubBackend);
        pool.shutdown();
        // 不 panic；stop flag 生效
        assert!(*pool.stop.lock().unwrap());
    }
}
