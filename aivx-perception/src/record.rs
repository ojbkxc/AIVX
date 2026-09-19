//! T3 录像线程（DESIGN.md §3.3）。
//!
//! 独立 `ffmpeg -c copy -f segment`——与 T1/T2 物理隔离（I4），挂了不影响报警。
//! 段边界无 hook：T3 低频轮询目录 mtime 产生 `RecordingSegment` 事件 → 投影器建索引。
//! 参照：rebucca `recording/manager.py`（-c copy + 按天/容量清理）。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::bridge::PlaneBridge;
use aivx_events::{DeviceId, Event};

/// 录像配置。
pub struct RecordCfg {
    pub device_id: DeviceId,
    pub rtsp_url: String,
    /// 录像根目录（如 ~/.aivx/record）。
    pub base_dir: PathBuf,
    /// 分段时长秒（默认 600）。
    pub segment_secs: u32,
    pub ffmpeg: String,
}

impl RecordCfg {
    /// 设备目录：{base}/{device_id}/。设备 ID 已是 UUID（安全做目录名）。
    pub fn device_dir(&self) -> PathBuf {
        self.base_dir.join(sanitize(&self.device_id))
    }

    pub fn segment_pattern(&self) -> PathBuf {
        self.device_dir().join("seg_%05d.mp4")
    }

    pub fn ffmpeg_args(&self) -> Vec<String> {
        vec![
            "-rtsp_transport".into(),
            "tcp".into(),
            "-i".into(),
            self.rtsp_url.clone(),
            "-c".into(),
            "copy".into(), // 零转码（I4：CPU ≈ 0）
            "-f".into(),
            "segment".into(),
            "-segment_time".into(),
            self.segment_secs.to_string(),
            "-reset_timestamps".into(),
            "1".into(),
            "-strftime".into(),
            "1".into(),
            self.segment_pattern()
                .to_string_lossy()
                .into_owned()
                .replace("%05d", "%Y%m%d_%H%M%S"),
        ]
    }
}

/// 目录名消毒：只留字母数字-_（防路径注入）。
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// T3 录像循环。专用 OS 线程。
pub fn record_loop(cfg: RecordCfg, bridge: Arc<PlaneBridge>) {
    let _ = std::fs::create_dir_all(cfg.device_dir());
    let mut backoff_secs = 1u64;
    while !bridge.control.stop.load(Ordering::Relaxed) {
        if let Ok(mut child) = spawn_recorder(&cfg) {
            backoff_secs = 1;
            bridge.emit(Event::StreamUp {
                device_id: cfg.device_id.clone(),
                mono_ns: crate::mono_ns(),
            });
            // ffmpeg 前台跑：等它退出（断流/stop）
            let status = child.wait();
            // stop 触发的退出：直接返回（child 已结束）
            if bridge.control.stop.load(Ordering::Relaxed) {
                let _ = status;
                return;
            }
            // 崩了 → 退避重拉
        }
        // 退避（stop 500ms 粒度响应）
        let mut remaining = Duration::from_secs(backoff_secs.min(30));
        while remaining > Duration::ZERO && !bridge.control.stop.load(Ordering::Relaxed) {
            let step = remaining.min(Duration::from_millis(500));
            std::thread::sleep(step);
            remaining -= step;
        }
        if bridge.control.stop.load(Ordering::Relaxed) {
            return;
        }
        backoff_secs *= 2;
    }
}

fn spawn_recorder(cfg: &RecordCfg) -> std::io::Result<Child> {
    Command::new(&cfg.ffmpeg)
        .args(cfg.ffmpeg_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// 段索引扫描（DESIGN.md §3.3：无 hook，mtime 轮询）。
///
/// T3 外的低频 task 调用（控制面 10s 一次）；产出 `RecordingSegment` 事件。
/// 返回本次新发现的段（已按 mtime 排序）。
pub struct SegmentScanner {
    device_id: DeviceId,
    dir: PathBuf,
    seen: std::collections::HashSet<PathBuf>,
}

impl SegmentScanner {
    pub fn new(device_id: DeviceId, dir: PathBuf) -> Self {
        Self {
            device_id,
            dir,
            seen: Default::default(),
        }
    }

    /// 扫描新段。`emit` 每段回调（控制面组装事件）。
    pub fn scan(&mut self, mut emit: impl FnMut(Event)) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut found: Vec<(PathBuf, std::time::SystemTime, u64)> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "mp4").unwrap_or(false))
            .filter_map(|p| {
                let meta = std::fs::metadata(&p).ok()?;
                Some((p, meta.modified().ok()?, meta.len()))
            })
            .collect();
        found.sort_by_key(|(_, mtime, _)| *mtime);
        for (path, mtime, size) in found {
            if self.seen.insert(path.clone()) {
                let start_mono = mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                // 段时长：文件名 %Y%m%d_%H%M%S 解析（失败则 duration=0 由投影器容错）
                let dur = parse_duration_hint(&path).unwrap_or(0.0);
                emit(Event::RecordingSegment {
                    device_id: self.device_id.clone(),
                    file_path: path.to_string_lossy().into_owned(),
                    start_mono_ns: start_mono,
                    duration_secs: dur,
                });
                let _ = size;
            }
        }
    }
}

/// 文件名 seg_YYYYMMDD_HHMMSS.mp4 → 时长提示（按分段时间戳差不可得，
/// 单文件给保守 0；投影器回放按文件实际探测）。P0 简化。
fn parse_duration_hint(_path: &Path) -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 目录名消毒：特殊字符剔除。
    #[test]
    fn sanitize_dir_name() {
        assert_eq!(sanitize("cam-1_abc"), "cam-1_abc");
        assert_eq!(sanitize("../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize("a\\b:c"), "abc");
    }

    /// ffmpeg 参数：-c copy 零转码 + segment 分段。
    #[test]
    fn ffmpeg_record_args() {
        let cfg = RecordCfg {
            device_id: "dev-1".into(),
            rtsp_url: "rtsp://1.2.3.4/stream1".into(),
            base_dir: PathBuf::from("/tmp/rec"),
            segment_secs: 600,
            ffmpeg: "ffmpeg".into(),
        };
        let args = cfg.ffmpeg_args().join(" ");
        assert!(args.contains("-c copy"));
        assert!(args.contains("-f segment"));
        assert!(args.contains("-segment_time 600"));
        assert!(args.contains("/tmp/rec/dev-1/"));
    }

    /// 段扫描：发现新文件发事件一次，重复扫描不重发（幂等）。
    #[test]
    fn segment_scan_idempotent() {
        let dir = std::env::temp_dir().join(format!("aivx-seg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 造两个段文件
        std::fs::write(dir.join("seg_20260919_010101.mp4"), b"x").unwrap();
        std::fs::write(dir.join("seg_20260919_010201.mp4"), b"x").unwrap();

        let mut scanner = SegmentScanner::new("dev-1".into(), dir.clone());
        let mut first = Vec::new();
        scanner.scan(|ev| first.push(ev));
        assert_eq!(first.len(), 2, "首次扫描应发现 2 段");
        // 二次扫描：无新文件 → 不重发
        let mut second = Vec::new();
        scanner.scan(|ev| second.push(ev));
        assert!(second.is_empty(), "幂等：重复扫描不得重发");
        // 新增一段 → 只发新的
        std::fs::write(dir.join("seg_20260919_010301.mp4"), b"x").unwrap();
        let mut third = Vec::new();
        scanner.scan(|ev| third.push(ev));
        assert_eq!(third.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
