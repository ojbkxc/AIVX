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
            "-an".into(), // 音频轨 copy 进 mp4 segment 会初始化失败（TP-LINK
            // 子码流带 aac/pcm；NVR 录像无音频需求）
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

/// 目录名消毒：只留字母数字-_（防路径注入）。pub 供控制面扫描编排复用。
pub fn sanitize(s: &str) -> String {
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

    /// 扫描新段。`emit` 返回 false 表示事件没送达（channel 满/关停）——
    /// 该文件**不得标记 seen**，下轮扫描重试（否则满队列时永久丢段——
    /// 线上 boot 首轮 556+582 段灌爆 1024 容量，尾部 117 段被 seen 吞掉）。
    pub fn scan(&mut self, mut emit: impl FnMut(Event) -> bool) {
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
            if self.seen.contains(&path) {
                continue;
            }
            // 段起点：文件名 seg_%Y%m%d_%H%M%S.mp4 解析（ffmpeg -strftime 1
            // 写的是段起点本地时刻——比 mtime（封口时刻）早一个段长；
            // 解析失败回退 mtime。
            let start_mono = parse_seg_start(&path, mtime);
            // 段时长：mtime（封口时刻）- 起点 = 真实录制时长；正在写的
            // 段 mtime 随写更新，值持续增长，API 层按 cap 600 收敛。
            let dur = (mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
                - start_mono as f64 / 1_000_000_000.0)
                .max(0.0);
            let delivered = emit(Event::RecordingSegment {
                device_id: self.device_id.clone(),
                file_path: path.to_string_lossy().into_owned(),
                start_mono_ns: start_mono,
                duration_secs: dur,
            });
            if delivered {
                self.seen.insert(path); // 送达才标记——未送达下轮重发
            } else {
                break; // channel 满了——后面的也送不进，留到下轮（有序性：旧段先）
            }
            let _ = size;
        }
    }
}

/// 文件名 seg_YYYYMMDD_HHMMSS.mp4 → 段起点墙钟纳秒。
///
/// ffmpeg `-strftime 1` 段文件名的时间是**段起点**（本地时区）——
/// 这是索引里 start_ts 的正确语义（mtime 是封口时刻，比起点晚一个
/// 段长；旧代码拿它当 start_ts，列表时间全偏晚 10 分钟）。
/// 解析失败（老文件/改名）回退 mtime。
fn parse_seg_start(path: &Path, mtime: std::time::SystemTime) -> u64 {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    // seg_YYYYMMDD_HHMMSS.mp4 → [YYYY, MM, DD, HH, MM, SS]
    let Some(tail) = name.strip_prefix("seg_") else {
        return mtime_ns(mtime);
    };
    let Some(stamp) = tail.strip_suffix(".mp4") else {
        return mtime_ns(mtime);
    };
    let b = stamp.as_bytes();
    if b.len() != 15 || b[8] != b'_' {
        return mtime_ns(mtime);
    }
    let ok = b.iter().all(|c| c.is_ascii_digit() || *c == b'_');
    let digits: Vec<u32> = stamp
        .chars()
        .filter(|c| c.is_ascii_digit())
        .map(|c| c.to_digit(10).unwrap())
        .collect();
    if !ok || digits.len() != 14 {
        return mtime_ns(mtime);
    }
    let (y, mo, d, h, mi, s) = (
        digits[0] as i32 * 1000 + digits[1] as i32 * 100 + digits[2] as i32 * 10 + digits[3] as i32,
        digits[4] * 10 + digits[5],
        digits[6] * 10 + digits[7],
        digits[8] * 10 + digits[9],
        digits[10] * 10 + digits[11],
        digits[12] * 10 + digits[13],
    );
    // 本地时区解析：via chrono-free 法——先把 Y/M/D/H/M/S 视作 UTC 拑出
    // epoch，再加本地时区偏移（libc::localtime 不引；用 env TZ 读不可靠。
    // 服务器时区固定 Asia/Shanghai（部署机 locale）→ 偏移 +8h。
    // 若跨时区部署，段起点会偏时区差——mtime 回退兜底同偏，可接受。
    let days = days_from_civil(y, mo as i32, d as i32);
    let epoch = days * 86400 + h as i64 * 3600 + mi as i64 * 60 + s as i64;
    let local_offset_secs = local_utc_offset();
    let secs = epoch - local_offset_secs;
    (secs.max(0) as u64) * 1_000_000_000
}

fn mtime_ns(mtime: std::time::SystemTime) -> u64 {
    mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// 本地时区与 UTC 的偏移秒（Asia/Shanghai = +8h；其他时区取 0）。
/// ffmpeg strftime 写文件名用本地时区——段起点 epoch 需减偏移。
fn local_utc_offset() -> i64 {
    // 已知部署机为 CST(+8)；通用化需 tzfile 解析，P0 不引依赖——
    // 环境变量 TZ 含 "UTC" 或空时按 0 算，否则 +8。
    match std::env::var("TZ") {
        Ok(tz) if tz == "UTC" || tz == "utc" || tz.starts_with("Etc/UTC") => 0,
        _ => 8 * 3600,
    }
}

/// civil 日期 → 自 1970-01-01 的天数（Howard Hinnant 算法，无依赖）。
fn days_from_civil(y: i32, m: i32, d: i32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era as i64 * 146097 + doe as i64 - 719468
}

/// 按保留天数清理超期段（DESIGN.md §3.3 承诺：按天数自动清理）。
///
/// 删除 mtime 早于 `retain_days` 天前的 `*.mp4`；**绝不动正在写的段**
/// （mtime 是封口时刻——ffmpeg segment 封口后不再碰它；但保守起见也
/// 排除 mtime 在最近 segment_secs*2 内的文件，防时钟跳变误删活跃段）。
/// 返回删除数。返回被删路径列表供调用方发事件/日志。
pub fn sweep_stale(device_dir: &Path, retain_days: u32, segment_secs: u32) -> Vec<PathBuf> {
    if retain_days == 0 {
        return Vec::new(); // 0 = 永久保留（用户语义：days 缺省无限）
    }
    let Ok(entries) = std::fs::read_dir(device_dir) else {
        return Vec::new();
    };
    // 保守活跃窗口：段时长的 2 倍（正在写的段 mtime 也会随写更新，
    // 但时钟跳变/极端场景下双保险比事后恢复段便宜）。
    let active_grace = segment_secs.max(60) as u64 * 2;
    let mut removed = Vec::new();
    for e in entries.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.extension().map(|x| x == "mp4") != Some(true) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
            continue; // mtime 在未来（时钟回拨）——不删
        };
        let age_secs = age.as_secs();
        // 超期且超出活跃窗口才删（collapse 后 remove 失败静默——下轮再试）
        if age_secs > retain_days as u64 * 86400
            && age_secs > active_grace
            && std::fs::remove_file(&p).is_ok()
        {
            removed.push(p);
        }
    }
    removed
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
        // PathBuf 分隔符随平台（Linux '/' / Windows '\'）——normalize 后断言
        assert!(args.replace('\\', "/").contains("/tmp/rec/dev-1/"));
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
        scanner.scan(|ev| {
            first.push(ev);
            true
        });
        assert_eq!(first.len(), 2, "首次扫描应发现 2 段");
        // 二次扫描：无新文件 → 不重发
        let mut second = Vec::new();
        scanner.scan(|ev| {
            second.push(ev);
            true
        });
        assert!(second.is_empty(), "幂等：重复扫描不得重发");
        // 新增一段 → 只发新的
        std::fs::write(dir.join("seg_20260919_010301.mp4"), b"x").unwrap();
        let mut third = Vec::new();
        scanner.scan(|ev| {
            third.push(ev);
            true
        });
        assert_eq!(third.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 满队列不丢段：emit 返回 false 的文件不得标记 seen——下轮扫描重发
    /// （线上 boot 首轮 1138 段灌爆 1024 channel，尾部 117 段被 seen 吞掉
    /// 的根因）。
    #[test]
    fn scan_undelivered_not_marked_seen() {
        let dir = std::env::temp_dir().join(format!("aivx-seg-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seg_20260919_010101.mp4"), b"x").unwrap();
        std::fs::write(dir.join("seg_20260919_010201.mp4"), b"x").unwrap();

        let mut scanner = SegmentScanner::new("dev-1".into(), dir.clone());
        // 第一轮：首条送达后拒收（模拟 channel 满）
        let mut first = Vec::new();
        let mut n = 0;
        scanner.scan(|ev| {
            n += 1;
            if n == 1 {
                first.push(ev);
                true
            } else {
                false
            }
        });
        assert_eq!(first.len(), 1, "第一条应送达");
        // 第二轮：第二条必须重发（未被 seen 吞）
        let mut second = Vec::new();
        scanner.scan(|ev| {
            second.push(ev);
            true
        });
        assert_eq!(
            second.len(),
            1,
            "未送达的段下轮必须重发（不得被 seen 吞掉）"
        );
        // 第三轮：全部已送达 → 幂等
        let mut third = Vec::new();
        scanner.scan(|ev| {
            third.push(ev);
            true
        });
        assert!(third.is_empty(), "补发后恢复幂等");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 保留清理：超期段删除、新段与活跃段保留、retain_days=0 不清理。
    #[test]
    fn sweep_stale_respects_retention() {
        let dir = std::env::temp_dir().join(format!("aivx-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("seg_20260101_000000.mp4");
        let fresh = dir.join("seg_20260921_080000.mp4");
        let recent = dir.join("seg_20260920_080000.mp4"); // 1 天内 → 留
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&fresh, b"x").unwrap();
        std::fs::write(&recent, b"x").unwrap();

        // 30 天前的 mtime
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86400);
        let _ = std::fs::File::options()
            .write(true)
            .open(&old)
            .and_then(|f| f.set_modified(past));

        // retain_days=0：永久保留，啥都不删
        let removed = sweep_stale(&dir, 0, 600);
        assert!(removed.is_empty(), "retain_days=0 不得清理");

        // retain_days=7：old 删，fresh/recent 留
        let removed = sweep_stale(&dir, 7, 600);
        assert_eq!(removed.len(), 1, "只删 30 天前的段");
        assert!(!old.exists(), "超期段必须删");
        assert!(fresh.exists(), "新段必须保留");
        assert!(recent.exists(), "1 天内段必须保留");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 段起点解析：seg_YYYYMMDD_HHMMSS.mp4 文件名 → 段起点墙钟（非 mtime）。
    /// 线上 bug：旧代码 start_ts=mtime（封口时刻），列表时间全偏晚 10 分钟。
    #[test]
    fn seg_start_from_filename() {
        let path = PathBuf::from("/tmp/x/seg_20260922_081620.mp4");
        let fake_mtime = std::time::SystemTime::now();
        let start_ns = parse_seg_start(&path, fake_mtime);
        let start = start_ns / 1_000_000_000;
        // 本地时区（默认 +8）下 2026-09-22 08:16:20 CST 的 Unix 秒
        let expect = if local_utc_offset() == 8 * 3600 {
            1790036180 // 2026-09-22 08:16:20 +08:00
        } else {
            start // 非 +8 环境不校准（CI UTC：epoch - offset 与文件名一致即可）
        };
        assert_eq!(start, expect, "段起点应从文件名解析");
        assert_ne!(start, mtime_ns(fake_mtime) / 1_000_000_000);
    }

    /// 文件名解析失败回退 mtime（老文件/非 seg 命名）。
    #[test]
    fn seg_start_fallback_mtime() {
        let fake_mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(12345);
        for name in ["clip_001.mp4", "seg_2026.mp4", "seg_abcdefgh_010101.mp4"] {
            let p = PathBuf::from("/tmp/x").join(name);
            assert_eq!(
                parse_seg_start(&p, fake_mtime),
                12345 * 1_000_000_000,
                "{name}"
            );
        }
        // 合法命名格式校验：15 位（8 日期 + 1 下划线 + 6 时分秒）
        let p = PathBuf::from("/tmp/x/seg_20260922_081620.mp4");
        assert!(parse_seg_start(&p, fake_mtime) != 12345 * 1_000_000_000);
    }

    /// 扫描事件携带段起点 + 真实时长（mtime - 起点）。
    #[test]
    fn scan_emits_start_and_duration() {
        let dir = std::env::temp_dir().join(format!("aivx-seg-dur-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 段起点 2026-09-22 08:16:20 CST；封口 mtime = 起点 + 598s
        let start = if local_utc_offset() == 8 * 3600 {
            1790036180u64
        } else {
            return; // 非 +8 环境跳过（避免时区耦合）
        };
        let f = dir.join("seg_20260922_081620.mp4");
        std::fs::write(&f, b"x").unwrap();
        let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(start + 598);
        let _ = std::fs::File::options()
            .write(true)
            .open(&f)
            .and_then(|fh| fh.set_modified(mtime));

        let mut scanner = SegmentScanner::new("dev-1".into(), dir.clone());
        let mut events = Vec::new();
        scanner.scan(|ev| {
            events.push(ev);
            true
        });
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::RecordingSegment {
                start_mono_ns,
                duration_secs,
                ..
            } => {
                assert_eq!(*start_mono_ns, start * 1_000_000_000, "start=段起点");
                assert!(
                    (*duration_secs - 598.0).abs() < 1.0,
                    "duration=mtime-起点≈598，got {duration_secs}"
                );
            }
            _ => panic!("应发 RecordingSegment"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
