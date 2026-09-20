//! 预览链路（DESIGN.md §8）：按需启停的 fMP4 分发（控制面，tokio）。
//!
//! WS 订阅计数 0→1 起预览 ffmpeg（`-c copy` 零转码，I4：与 T1/T2 物理隔离，
//! 预览挂不影响报警）；1→0 kill。产出经 box 解析成 init/media segment，broadcast
//! 给所有订阅者；init + 最近一段 media 缓存，新客户端秒出画面。
//! moov codec 是 HEVC 时（浏览器 MSE 不收）自动换 libx264 转码重启。
//! 看门狗：3s 无输出 kill 重拉。
//!
//! 参照：ai-nvr H264Fmp4Extractor（常驻拉流）+ api/index.ts（WS 分发/背压）。
//! 差异：AIVX 按需启停（DESIGN.md 明确要求，ruoyi-zlm on_stream_none_reader 语义）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::sync::{broadcast, Mutex};

use crate::fmp4::{codec_is_hevc, Fmp4Chunk, Fmp4Parser};

/// 预览 WS 二进制帧 tag（协议与 ai-nvr 一致，见 frontend/src/lib/fmp4-player.ts）。
const TYPE_INIT: u8 = 0x01;
const TYPE_MEDIA: u8 = 0x02;

/// broadcast 容量：约 8s 的 720p 段（慢客户端 Lagged 跳最新，前端 catchUp 兜底）。
const CHANNEL_CAP: usize = 64;
/// 看门狗：stdout 静默超过此时长 kill 重拉。
const WATCHDOG: Duration = Duration::from_secs(3);
/// ffmpeg 重启退避封顶（连续失败时降频，抄 ai-nvr）。
const RESTART_CAP: Duration = Duration::from_secs(30);

/// 每路摄像头的预览流（一个 ffmpeg + 一条 broadcast）。
pub struct PreviewStream {
    id: String,
    rtsp_url: String,
    ffmpeg: String,
    /// 已编码 WS 消息的广播（Arc<PreviewStream> 内共享）。
    tx: broadcast::Sender<Arc<Vec<u8>>>,
    /// ffmpeg 会话生命周期（含按需启停的互斥）。
    lifecycle: Mutex<StreamLifecycle>,
}

struct StreamLifecycle {
    subscribers: usize,
    task: Option<tokio::task::JoinHandle<()>>,
    child: Option<Arc<Mutex<Child>>>,
    /// 新客户端秒开缓存（连接时先发这两条）。
    cached_init: Option<Arc<Vec<u8>>>,
    cached_media: Option<Arc<Vec<u8>>>,
}

impl PreviewStream {
    fn new(id: String, rtsp_url: String, ffmpeg: String) -> Arc<Self> {
        let (tx, _) = broadcast::channel(CHANNEL_CAP);
        Arc::new(Self {
            id,
            rtsp_url,
            ffmpeg,
            tx,
            lifecycle: Mutex::new(StreamLifecycle {
                subscribers: 0,
                task: None,
                child: None,
                cached_init: None,
                cached_media: None,
            }),
        })
    }

    /// 新订阅者：缓存引用 + 计数 0→1 拉起 ffmpeg。返回接收端 + 秒开缓存。
    pub async fn subscribe(
        self: &Arc<Self>,
    ) -> (broadcast::Receiver<Arc<Vec<u8>>>, Option<Arc<Vec<u8>>>, Option<Arc<Vec<u8>>>) {
        let mut lc = self.lifecycle.lock().await;
        lc.subscribers += 1;
        if lc.subscribers == 1 {
            let stream = Arc::clone(self);
            let task = tokio::spawn(async move { stream.run().await });
            lc.task = Some(task);
        }
        let rx = self.tx.subscribe();
        (rx, lc.cached_init.clone(), lc.cached_media.clone())
    }

    /// 订阅者断开：计数 1→0 停 ffmpeg（按需启停）。
    pub async fn unsubscribe(self: &Arc<Self>) {
        let mut lc = self.lifecycle.lock().await;
        lc.subscribers = lc.subscribers.saturating_sub(1);
        if lc.subscribers == 0 {
            if let Some(task) = lc.task.take() {
                task.abort();
            }
            if let Some(child) = lc.child.take() {
                child.lock().await.kill().await.ok();
            }
            // 清缓存：下次订阅重新探测（codec 可能变）
            lc.cached_init = None;
            lc.cached_media = None;
        }
    }

    /// 常驻拉流循环：spawn ffmpeg → 读 stdout → box 解析 → broadcast + 缓存。
    /// HEVC init 自动切转码重启；退出/看门狗超时按退避重拉（仍有订阅者时）。
    async fn run(self: Arc<Self>) {
        let mut transcode = false; // HEVC 后切 true
        let mut backoff = Duration::from_secs(1);
        loop {
            // 订阅者清零即退（unsubscribe 已 kill child；此处退出 task）
            if self.lifecycle.lock().await.subscribers == 0 {
                return;
            }
            let args = Self::ffmpeg_args(&self.rtsp_url, transcode);
            let mut child = match Command::new(&self.ffmpeg)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(c) => c,
                Err(_) => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RESTART_CAP);
                    continue;
                }
            };
            let stdout = match child.stdout.take() {
                Some(s) => s,
                None => {
                    child.kill().await.ok();
                    tokio::time::sleep(backoff).await;
                    continue;
                }
            };
            {
                let mut lc = self.lifecycle.lock().await;
                lc.child = Some(Arc::new(Mutex::new(child)));
            }
            // 新会话 = 新 init（codec 可能变）。空帧广播是分隔标记：旧订阅者
            // 在 preview_session 里跳过空帧；真正的 init 随后作为正常帧到达。
            self.tx.send(Arc::new(Vec::new())).ok();
            let outcome = self.pump(stdout).await;
            {
                let mut lc = self.lifecycle.lock().await;
                if let Some(c) = lc.child.take() {
                    c.lock().await.kill().await.ok();
                }
            }
            match outcome {
                PumpOutcome::Hevc => {
                    transcode = true; // 切 libx264 重启（一次决策，不再回切）
                    backoff = Duration::from_secs(1);
                }
                PumpOutcome::Eof | PumpOutcome::Watchdog => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RESTART_CAP);
                }
            }
        }
    }

    /// 读 stdout 直到 EOF/看门狗/HEVC 检出。
    async fn pump(self: &Arc<Self>, mut stdout: tokio::process::ChildStdout) -> PumpOutcome {
        use tokio::io::AsyncReadExt;
        let mut parser = Fmp4Parser::new();
        let mut buf = vec![0u8; 128 * 1024];
        // 看门狗：deadline 制——每次收到数据顺延（interval 无 reset API，别用它）。
        let mut deadline = tokio::time::Instant::now() + WATCHDOG;
        loop {
            tokio::select! {
                read = stdout.read(&mut buf) => match read {
                    Ok(0) | Err(_) => return PumpOutcome::Eof,
                    Ok(n) => {
                        for chunk in parser.feed(&buf[..n]) {
                            match chunk {
                                Fmp4Chunk::Init { data, codec } => {
                                    if codec_is_hevc(&codec) {
                                        return PumpOutcome::Hevc;
                                    }
                                    let msg = Arc::new(encode_init(&codec, &data));
                                    let mut lc = self.lifecycle.lock().await;
                                    lc.cached_init = Some(Arc::clone(&msg));
                                    drop(lc);
                                    self.tx.send(msg).ok();
                                }
                                Fmp4Chunk::Media { data } => {
                                    let msg = Arc::new(encode_media(&data));
                                    let mut lc = self.lifecycle.lock().await;
                                    lc.cached_media = Some(Arc::clone(&msg));
                                    drop(lc);
                                    self.tx.send(msg).ok();
                                }
                            }
                        }
                        deadline = tokio::time::Instant::now() + WATCHDOG;
                    }
                },
                _ = tokio::time::sleep_until(deadline) => return PumpOutcome::Watchdog,
            }
        }
    }

    /// ffmpeg 参数：低延迟拉流 → fMP4。copy 零转码（CPU≈0）；HEVC 时 libx264
    /// superfast CRF23（DESIGN.md §8）。不 scale——监看不降分辨率（AGENTS.md Never-9）。
    fn ffmpeg_args(rtsp_url: &str, transcode: bool) -> Vec<String> {
        let mut args: Vec<String> = [
            "-rtsp_transport", "tcp",
            "-fflags", "nobuffer+genpts+discardcorrupt",
            "-flags", "low_delay",
            "-max_delay", "0",
            "-reorder_queue_size", "0",
            "-thread_queue_size", "1",
            "-analyzeduration", "100000",
            "-probesize", "32768",
            "-i",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.push(rtsp_url.to_string());
        args.push("-an".into());
        if transcode {
            args.extend([
                "-c:v", "libx264", "-preset", "superfast", "-tune", "zerolatency",
                "-crf", "23", "-g", "30", "-pix_fmt", "yuv420p",
            ]
            .iter()
            .map(|s| s.to_string()));
        } else {
            args.extend(["-c", "copy"].iter().map(|s| s.to_string()));
        }
        args.extend([
            "-f", "mp4",
            "-movflags", "frag_keyframe+empty_moov+default_base_moof",
            "-frag_duration", "1000000", // 1s 一段（timescale 1000）
            "-flush_packets", "1",
            "pipe:1",
        ]
        .iter()
        .map(|s| s.to_string()));
        args
    }
}

enum PumpOutcome {
    Eof,
    Watchdog,
    Hevc,
}

/// init 帧：`[0x01][2B LE codec_len][codec][2B LE audio_len=0][fMP4]`（ai-nvr 协议）。
fn encode_init(codec: &str, fmp4: &[u8]) -> Vec<u8> {
    let cb = codec.as_bytes();
    let mut msg = Vec::with_capacity(5 + cb.len() + fmp4.len());
    msg.push(TYPE_INIT);
    msg.extend_from_slice(&(cb.len() as u16).to_le_bytes());
    msg.extend_from_slice(cb);
    msg.extend_from_slice(&0u16.to_le_bytes()); // 无音频（预览 -an）
    msg.extend_from_slice(fmp4);
    msg
}

/// media 帧：`[0x02][moof+mdat]`。
fn encode_media(fmp4: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(1 + fmp4.len());
    msg.push(TYPE_MEDIA);
    msg.extend_from_slice(fmp4);
    msg
}

/// 全局预览注册表：device_id → PreviewStream（惰性创建）。
pub struct PreviewHub {
    streams: Mutex<HashMap<String, Arc<PreviewStream>>>,
    ffmpeg: String,
}

impl PreviewHub {
    pub fn new(ffmpeg: String) -> Self {
        Self {
            streams: Mutex::new(HashMap::new()),
            ffmpeg,
        }
    }

    /// 订阅一路预览。无此设备返回 None（路由层转 404）。
    pub async fn subscribe(
        &self,
        id: &str,
        rtsp_url: Option<&str>,
    ) -> Option<(broadcast::Receiver<Arc<Vec<u8>>>, Option<Arc<Vec<u8>>>, Option<Arc<Vec<u8>>>)> {
        let rtsp = rtsp_url?.to_string();
        let mut map = self.streams.lock().await;
        let stream = map
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(PreviewStream::new(id.into(), rtsp, self.ffmpeg.clone())))
            .clone();
        Some(stream.subscribe().await)
    }

    /// 退订（WS 断开必调——不退订则 ffmpeg 永不停止）。
    pub async fn unsubscribe(&self, id: &str) {
        let stream = self.streams.lock().await.get(id).cloned();
        if let Some(s) = stream {
            s.unsubscribe().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 帧编码：init 带 codec 前缀 + 长度字段；media 仅 tag 前缀。
    #[test]
    fn frame_encoding() {
        let init = encode_init("avc1.42C01E", b"FTYP_MOOV");
        assert_eq!(init[0], TYPE_INIT);
        assert_eq!(u16::from_le_bytes([init[1], init[2]]), 11);
        assert_eq!(&init[3..14], b"avc1.42C01E");
        assert_eq!(u16::from_le_bytes([init[14], init[15]]), 0);
        assert_eq!(&init[16..], b"FTYP_MOOV");
        let media = encode_media(b"MOOFMDAT");
        assert_eq!(media[0], TYPE_MEDIA);
        assert_eq!(&media[1..], b"MOOFMDAT");
    }

    /// ffmpeg 参数：copy 分支零转码 + fMP4 flags；转码分支 libx264 不缩放。
    #[test]
    fn ffmpeg_args_shape() {
        let copy = PreviewStream::ffmpeg_args("rtsp://u:p@1.2.3.4/stream1", false);
        let joined = copy.join(" ");
        assert!(joined.contains("-c copy"));
        assert!(joined.contains("frag_keyframe+empty_moov+default_base_moof"));
        assert!(joined.contains("pipe:1"));
        assert!(joined.contains("-an"));
        assert!(!joined.contains("scale="));
        let x264 = PreviewStream::ffmpeg_args("rtsp://u:p@1.2.3.4/stream1", true);
        let joined = x264.join(" ");
        assert!(joined.contains("libx264"));
        assert!(joined.contains("zerolatency"));
        assert!(!joined.contains("-c copy"));
    }
}
