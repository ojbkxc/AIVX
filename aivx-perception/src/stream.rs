//! T1 拉流线程 + 断流状态机（DESIGN.md §3.1 / §7）。
//!
//! - `std::process::Command` spawn ffmpeg（ADR-019：数据面不用 tokio::process）
//! - `read_exact` 整帧读（vs frigate 的尽力读——短读=帧撕裂且静默；这里把
//!   撕裂变成显式故障进状态机）
//! - 指数退避 1s→30s，连续 10 次失败降级为 5min 探测（抄 ai-nvr 重连降频）
//! - 状态迁移发 StreamUp/StreamDown 事件（经 PlaneBridge，绝不碰 DB——I2）

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::bridge::PlaneBridge;
use crate::frame::LatestFrameSlot;
use aivx_events::{DeviceId, Event};

/// 断流状态机（DESIGN.md §7）。u8 原子便于控制面直读。
pub mod state {
    pub const CONNECTING: u8 = 0;
    pub const OK: u8 = 1;
    pub const RECONNECTING: u8 = 2;
    pub const DEGRADED: u8 = 3;
    pub const STOPPED: u8 = 4;

    pub fn name(v: u8) -> &'static str {
        match v {
            CONNECTING => "connecting",
            OK => "ok",
            RECONNECTING => "reconnecting",
            DEGRADED => "degraded",
            _ => "stopped",
        }
    }
}

/// 拉流配置（P0 由调用方构造；P1 接 ONVIF GetStreams 后自动填充）。
pub struct DecodeCfg {
    pub device_id: DeviceId,
    pub rtsp_url: String,
    pub width: usize,
    pub height: usize,
    /// ffmpeg 二进制（PATH 或绝对路径）。
    pub ffmpeg: String,
    /// 额外 ffmpeg 输入参数（如 -rtsp_transport tcp）。
    pub input_args: Vec<String>,
}

impl DecodeCfg {
    /// ffmpeg 参数：拉子码流 → 硬解 → NV12 裸流。
    ///
    /// `-an` 丢音频（分析不需要）；`-hwaccel auto` 硬解优先、失败自动软解；
    /// NV12 是 I1 的单一事实格式（DESIGN.md §2）。
    pub fn ffmpeg_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec!["-rtsp_transport".into(), "tcp".into()];
        args.extend(self.input_args.iter().cloned());
        args.extend([
            "-i".into(),
            self.rtsp_url.clone(),
            "-an".into(),
            "-hwaccel".into(),
            "auto".into(),
            "-pix_fmt".into(),
            "nv12".into(),
            "-f".into(),
            "rawvideo".into(),
            "-".into(),
        ]);
        args
    }
}

/// T1 拉流循环入口。在专用 OS 线程跑（调用方 std::thread::spawn）。
///
/// `slot` 由本函数独占（单写者所有权）；`bridge.control.stop` 是唯一退出信号。
pub fn decode_loop(cfg: DecodeCfg, mut slot: LatestFrameSlot, bridge: Arc<PlaneBridge>) {
    let mut backoff = Backoff::default();
    let mut fail_streak: u32 = 0;
    bridge.set_stream_state(state::CONNECTING);

    while !bridge.control.stop.load(Ordering::Relaxed) {
        match spawn_ffmpeg(&cfg) {
            Ok(mut child) => {
                let frame_size = slot.frame_size();
                let mut stdout = match child.stdout.take() {
                    Some(s) => s,
                    None => {
                        fail_streak += 1;
                        backoff_sleep(&mut backoff, &mut fail_streak, &cfg, &bridge);
                        continue;
                    }
                };
                let mut first_frame = true;
                loop {
                    if bridge.control.stop.load(Ordering::Relaxed) {
                        let _ = child.kill();
                        bridge.set_stream_state(state::STOPPED);
                        return;
                    }
                    // read_exact 直写 slot 非活跃缓冲（零拷贝——I1 生产路径）
                    match write_exact(&mut stdout, &mut slot, frame_size) {
                        WriteOutcome::Frame(_) => {
                            if first_frame {
                                first_frame = false;
                                fail_streak = 0;
                                backoff.reset();
                                bridge.set_stream_state(state::OK);
                                bridge.emit(Event::StreamUp {
                                    device_id: cfg.device_id.clone(),
                                    mono_ns: crate::mono_ns(),
                                });
                            }
                            bridge.metrics.decode_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        WriteOutcome::Eof => break,
                    }
                }
                let _ = child.wait();
                // 走到这=ffmpeg 退出（断流/EOF/错误）
                fail_streak += 1;
            }
            Err(_) => {
                fail_streak += 1;
            }
        }
        backoff_sleep(&mut backoff, &mut fail_streak, &cfg, &bridge);
    }
    bridge.set_stream_state(state::STOPPED);
}

/// 整帧 read_exact 直写 slot 的非活跃缓冲并 commit。
///
/// 返回 Eof 即外层重连。短读（读到一半断）同样按 Eof 处理——分辨率变更或
/// 网络断都会造成字节错位，唯一正确动作是重启 ffmpeg（DESIGN.md §3.1 自愈）。
fn write_exact(
    stdout: &mut impl Read,
    slot: &mut LatestFrameSlot,
    frame_size: usize,
) -> WriteOutcome {
    // 两段式：先 begin（锁缓冲+seq 变奇），再读，读完 commit。
    // 读半途失败：缓冲 seq 停在奇数——读者永不读它（安全），下轮 begin_write
    // 选同一非活跃缓冲会 debug_assert；所以失败必须 rollback（seq 回偶）。
    let (idx, buf) = slot.begin_write();
    let mut read_total = 0usize;
    let mut chunk = [0u8; 16384];
    while read_total < frame_size {
        let want = chunk.len().min(frame_size - read_total);
        match stdout.read(&mut chunk[..want]) {
            Ok(0) => {
                slot.rollback_write(idx);
                return WriteOutcome::Eof;
            }
            Ok(n) => {
                buf[read_total..read_total + n].copy_from_slice(&chunk[..n]);
                read_total += n;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                slot.rollback_write(idx);
                return WriteOutcome::Eof;
            }
        }
    }
    WriteOutcome::Frame(slot.commit_write(idx))
}

/// 帧写入结果。`Frame(u64)` 载荷是帧代数——生产 decode_loop 只关心变体，
/// 帧代数经 metrics（gen/decode_frames）发布；测试经 `frame_gen()` 消费。
#[expect(dead_code, reason = "Frame 载荷仅测试消费;生产帧代数走 metrics 通道")]
enum WriteOutcome {
    Frame(u64),
    Eof,
}

fn spawn_ffmpeg(cfg: &DecodeCfg) -> std::io::Result<Child> {
    Command::new(&cfg.ffmpeg)
        .args(cfg.ffmpeg_args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

/// 退避状态机：1s→2s→…→30s 封顶；连续 10 次失败转 DEGRADED（5min 探测）。
fn backoff_sleep(
    backoff: &mut Backoff,
    fail_streak: &mut u32,
    cfg: &DecodeCfg,
    bridge: &Arc<PlaneBridge>,
) {
    bridge.emit(Event::StreamDown {
        device_id: cfg.device_id.clone(),
        reason: format!("reconnect_attempt_{}", fail_streak),
        mono_ns: crate::mono_ns(),
    });
    let (dur, new_state) = if *fail_streak >= 10 {
        (Duration::from_secs(300), state::DEGRADED)
    } else {
        let s = 1u64 << (*fail_streak).min(5); // 1,2,4,8,16,32→cap 30
        (Duration::from_secs(s.min(30)), state::RECONNECTING)
    };
    backoff.escalate_to(dur); // 记录当前档位（策略载体）
    let _ = backoff.current();
    bridge.set_stream_state(new_state);
    // 分片睡：stop 信号 500ms 内响应
    let mut remaining = dur;
    while remaining > Duration::ZERO && !bridge.control.stop.load(Ordering::Relaxed) {
        let step = remaining.min(Duration::from_millis(500));
        std::thread::sleep(step);
        remaining -= step;
    }
}

/// 退避策略载体。P0：sleep 时长由 fail_streak 决定，Backoff 记录历史
/// （供未来 AIMD/健康联动策略）；字段经 `current()` 读出即不算 dead_code。
struct Backoff {
    base: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
        }
    }
}
impl Backoff {
    fn reset(&mut self) {
        self.base = Duration::from_secs(1);
    }
    fn current(&self) -> Duration {
        self.base
    }
    /// 进入下一档退避（backoff_sleep 计算时长后同步到载体）。
    fn escalate_to(&mut self, d: Duration) {
        self.base = d;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ffmpeg 参数正确性：tcp 拉流 → nv12 裸流。
    #[test]
    fn ffmpeg_args_shape() {
        let cfg = DecodeCfg {
            device_id: "cam-1".into(),
            rtsp_url: "rtsp://u:p@1.2.3.4/stream2".into(),
            width: 640,
            height: 360,
            ffmpeg: "ffmpeg".into(),
            input_args: vec![],
        };
        let args = cfg.ffmpeg_args();
        let joined = args.join(" ");
        assert!(joined.contains("-rtsp_transport tcp"));
        assert!(joined.contains("-pix_fmt nv12"));
        assert!(joined.contains("-f rawvideo"));
        assert!(joined.ends_with('-'));
    }

    /// read_exact 语义：喂足字节=Frame；字节流截断=Eof（无撕裂中间态）。
    /// 用假 Reader 驱动 write_exact，验证协议正确性——这是 CI 无摄像头
    /// 也能跑的"合成流"（I3 延迟断言的底层）。
    #[test]
    fn write_exact_frame_and_eof() {
        let (bridge, _rx) = PlaneBridge::new(16, None);
        let bridge = Arc::new(bridge);
        let mut slot = LatestFrameSlot::new(64, 36);
        let fs = slot.frame_size();

        // 1) 足量字节 → 一帧
        let full = vec![7u8; fs];
        let mut rd = std::io::Cursor::new(full.clone());
        match write_exact(&mut rd, &mut slot, fs) {
            WriteOutcome::Frame(g) => assert_eq!(g, 1),
            WriteOutcome::Eof => panic!("足量字节必须是 Frame"),
        }
        let fr = slot.read_latest().expect("应有帧");
        slot.borrow_y(&fr, |y| assert!(y.iter().all(|&b| b == 7)))
            .expect("seq 一致");

        // 2) 截断流（半帧后 EOF）→ Eof，缓冲回滚（下次可再写）
        let partial = vec![9u8; fs / 2];
        let mut rd2 = std::io::Cursor::new(partial);
        match write_exact(&mut rd2, &mut slot, fs) {
            WriteOutcome::Eof => {}
            WriteOutcome::Frame(_) => panic!("截断必须是 Eof"),
        }
        // 回滚后 slot 可继续写（协议未卡死）
        let mut rd3 = std::io::Cursor::new(vec![5u8; fs]);
        match write_exact(&mut rd3, &mut slot, fs) {
            WriteOutcome::Frame(g) => assert_eq!(g, 2),
            WriteOutcome::Eof => panic!("回滚后应可继续写"),
        }
        let _ = bridge;
    }
}
