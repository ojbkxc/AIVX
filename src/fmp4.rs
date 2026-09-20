//! fMP4 字节流解析（预览链路，DESIGN.md §8）。
//!
//! ffmpeg 以 `-f mp4 -movflags frag_keyframe+empty_moov+default_base_moof` 输出
//! fMP4 到 stdout；本模块按 ISO BMFF box 边界（`[4B size][4B type]`）切分：
//! - init segment = `ftyp` + `moov`（缓存供新客户端秒开）
//! - media segment = 相邻的 `moof` + `mdat`
//! codec 字符串从 moov 的 `stsd → avc1/hvc1 → avcC/hvcC` 提取（MSE 需要）。
//!
//! 参照：ai-nvr `h264-fmp4-muxer.ts`（Fmp4StreamParser）。差异：不做 tfdt 重写——
//! AIVX 前端 catchUpToLive（延迟 >2s 强制 seek）兜底播放速率。

/// 解析产出：一次喂入可能产出 init 和/或若干 media segment。
#[derive(Debug, Default)]
pub struct Fmp4Parser {
    /// 残余未解析字节（box 不完整时续读）。
    buf: Vec<u8>,
    /// 已收集 init（true 后进入 media 分支）。
    init_collected: bool,
    /// init 收集期间累积的完整 box（ftyp…moov）。
    init_boxes: Vec<(String, Vec<u8>)>,
    /// media 收集期间累积的完整 box（moof…mdat）。
    media_boxes: Vec<(String, Vec<u8>)>,
    /// moof 之后是否见过 mdat（moof+mdat 相邻成段）。
    saw_moof: bool,
}

/// 一段解析产物。
#[derive(Debug, PartialEq)]
pub enum Fmp4Chunk {
    /// init segment（ftyp+moov 拼接）+ codec 字符串（如 "avc1.42C01E"）。
    Init { data: Vec<u8>, codec: String },
    /// media segment（moof+mdat 拼接）。
    Media { data: Vec<u8> },
}

impl Fmp4Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入 stdout 字节，返回本次产出的 chunk 列表（可能为空——box 未完整）。
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Fmp4Chunk> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let Some((box_type, payload, consumed)) = parse_next_box(&self.buf) else {
                break;
            };
            self.buf.drain(..consumed);
            self.handle_box(box_type, payload, &mut out);
        }
        out
    }

    fn handle_box(&mut self, box_type: String, payload: Vec<u8>, out: &mut Vec<Fmp4Chunk>) {
        if !self.init_collected {
            self.init_boxes.push((box_type.clone(), payload));
            if box_type == "moov" {
                let mut data = Vec::new();
                for (_, p) in &self.init_boxes {
                    data.extend_from_slice(p);
                }
                let codec = extract_codec(&data).unwrap_or_else(|| "avc1.42C01E".into());
                self.init_collected = true;
                self.init_boxes.clear();
                out.push(Fmp4Chunk::Init { data, codec });
            }
            return;
        }
        if box_type == "moof" {
            self.saw_moof = true;
        }
        self.media_boxes.push((box_type, payload));
        if box_type == "mdat" && self.saw_moof {
            let mut data = Vec::new();
            for (_, p) in &self.media_boxes {
                data.extend_from_slice(p);
            }
            self.media_boxes.clear();
            self.saw_moof = false;
            out.push(Fmp4Chunk::Media { data });
        }
    }
}

/// 解析 buffer 头部一个完整 box。返回 (type, 含 box 头的完整字节, 总长)。
/// 不完整返回 None（等更多字节）。
fn parse_next_box(buf: &[u8]) -> Option<(String, Vec<u8>, usize)> {
    if buf.len() < 8 {
        return None;
    }
    let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let box_type = String::from_utf8_lossy(&buf[4..8]).into_owned();
    if size == 1 {
        // 64 位扩展长度（mdat 超大时出现）
        if buf.len() < 16 {
            return None;
        }
        let big = u64::from_be_bytes(buf[8..16].try_into().ok()?) as usize;
        if buf.len() < big {
            return None;
        }
        return Some((box_type, buf[..big].to_vec(), big));
    }
    if size == 0 {
        // size 0 = 到流末尾——对管道流当"剩余全部"处理
        if buf.len() < 8 {
            return None;
        }
        let all = buf.len();
        return Some((box_type, buf.to_vec(), all));
    }
    if size < 8 || buf.len() < size {
        return None;
    }
    Some((box_type, buf[..size].to_vec(), size))
}

/// 从 init segment（ftyp+moov）提取 codec 字符串。
/// 深搜 moov→trak→mdia→minf→stbl→stsd→avc1/hvc1→avcC/hvcC。
pub fn extract_codec(init: &[u8]) -> Option<String> {
    // moov 容器递归：找到 stsd 再看 sample entry
    let moov = find_box(init, b"moov")?;
    let stsd = find_box_deep(moov, b"stsd")?;
    // stsd payload: [4B version/flags][4B entry_count][entries...]
    if stsd.len() < 8 {
        return None;
    }
    let entries = &stsd[8..];
    for fourcc in [b"avc1", b"hvc1", b"hev1"] {
        if let Some(entry) = find_box(entries, fourcc) {
            if fourcc == b"avc1" {
                if let Some(avcc) = find_box(entry, b"avcC") {
                    if avcc.len() >= 5 {
                        // avcC: [0][profile][compat][level]...
                        let p = avcc[1];
                        let c = avcc[2];
                        let l = avcc[3];
                        return Some(format!("avc1.{:02X}{:02X}{:02X}", p, c, l));
                    }
                }
            } else {
                if let Some(hvcc) = find_box(entry, b"hvcC") {
                    if hvcc.len() >= 13 {
                        // hvcC: [0][profileIdc][compatFlags ×4]…[12]=levelIdc
                        let profile = hvcc[1];
                        let level = hvcc[12];
                        return Some(format!(
                            "{}.{:x}.L{:X}.B0",
                            String::from_utf8_lossy(fourcc),
                            profile,
                            level
                        ));
                    }
                }
            }
        }
    }
    None
}

/// 在容器 payload 里找一层子 box，返回其 payload。
fn find_box(payload: &[u8], ty: &[u8; 4]) -> Option<&[u8]> {
    let mut off = 0usize;
    while off + 8 <= payload.len() {
        let size = u32::from_be_bytes(payload[off..off + 4].try_into().ok()?) as usize;
        if size < 8 || off + size > payload.len() {
            return None;
        }
        if &payload[off + 4..off + 8] == ty {
            return Some(&payload[off + 8..off + size]);
        }
        off += size;
    }
    None
}

/// 递归深搜一层容器树（moov→trak→…→stsd）。
fn find_box_deep(payload: &[u8], ty: &[u8; 4]) -> Option<&[u8]> {
    let mut off = 0usize;
    while off + 8 <= payload.len() {
        let size = u32::from_be_bytes(payload[off..off + 4].try_into().ok()?) as usize;
        if size < 8 || off + size > payload.len() {
            return None;
        }
        let cur_ty = &payload[off + 4..off + 8];
        let inner = &payload[off + 8..off + size];
        if cur_ty == ty {
            return Some(inner);
        }
        // trak/mdia/minf/stbl 是容器：递归找
        if matches!(cur_ty, b"trak" | b"mdia" | b"minf" | b"stbl") {
            if let Some(found) = find_box_deep(inner, ty) {
                return Some(found);
            }
        }
        off += size;
    }
    None
}

/// 判断 codec 是否 HEVC（触发预览 ffmpeg 切转码重启）。
pub fn codec_is_hevc(codec: &str) -> bool {
    codec.starts_with("hvc1") || codec.starts_with("hev1")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 box：[4B size][4B type][payload]。
    fn box_(ty: &str, payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        b.extend_from_slice(ty.as_bytes());
        b.extend_from_slice(payload);
        b
    }

    /// 构造带 avcC/hvcC 的完整 init（ftyp + moov[trak[mdia[minf[stbl[stsd[entry[cc]]]]]]]）。
    /// avcC 的 profile/compat/level 标在 [1][2][3] 位置；hvcC 的 level 在 [12]。
    fn synthetic_init(profile: u8, compat: u8, level: u8, fourcc: &str, cc_fourcc: &str) -> Vec<u8> {
        let avcc = box_(
            cc_fourcc,
            &[
                0x01, profile, compat, level, 0xff, 0xe1, 0x00, 0x08, 0x67, profile, compat,
                level, 0xde, 0xad,
            ],
        );
        // sample entry：前部固定占位字节 + 编码配置 box（解析器只找 cc box）
        let entry = box_(fourcc, &[0u8; 6, 0xff].iter().chain(avcc.iter()).cloned().collect::<Vec<u8>>().as_slice());
        let stsd = box_("stsd", &[0, 0, 0, 0, 0, 0, 0, 1].iter().chain(entry.iter()).cloned().collect::<Vec<u8>>().as_slice());
        let stbl = box_("stbl", &stsd);
        let minf = box_("minf", &stbl);
        let mdia = box_("mdia", &minf);
        let trak = box_("trak", &mdia);
        let moov = box_("moov", &trak);
        let ftyp = box_("ftyp", b"isom");
        let mut all = ftyp;
        all.extend_from_slice(&moov);
        all
    }

    #[test]
    fn parse_init_then_media() {
        let mut p = Fmp4Parser::new();
        let init = synthetic_init(0x42, 0xC0, 0x1E, "avc1", "avcC");
        let mut stream = init.clone();
        stream.extend_from_slice(&box_("moof", &[1, 2, 3]));
        stream.extend_from_slice(&box_("mdat", &[4, 5, 6]));
        let chunks = p.feed(&stream);
        assert_eq!(chunks.len(), 2, "init + 1 media");
        match &chunks[0] {
            Fmp4Chunk::Init { data, codec } => {
                assert_eq!(data, &init, "init = ftyp+moov 原样");
                assert_eq!(codec, "avc1.42C01E");
            }
            Fmp4Chunk::Media { .. } => panic!("第一段必须是 init"),
        }
        assert!(matches!(&chunks[1], Fmp4Chunk::Media { data } if data.len() == 8 + 3 + 8 + 3));
    }

    /// 半包续读：box 字节分两次喂入，必须无丢失地解析出来。
    #[test]
    fn partial_box_across_feeds() {
        let mut p = Fmp4Parser::new();
        let init = synthetic_init(0x42, 0xC0, 0x1E, "avc1", "avcC");
        let media = box_("moof", &[1]).iter().chain(box_("mdat", &[2]).iter()).cloned().collect::<Vec<u8>>();
        let mut full = init.clone();
        full.extend_from_slice(&media);
        // 在 media 正中间劈开
        let split = init.len() + 5;
        let (a, b) = full.split_at(split);
        let c1 = p.feed(a);
        let c2 = p.feed(b);
        let total = c1.len() + c2.len();
        assert_eq!(total, 2, "跨 feed 必须凑齐 init+media");
        assert!(c1.iter().any(|c| matches!(c, Fmp4Chunk::Init { .. })));
        assert!(c2.iter().any(|c| matches!(c, Fmp4Chunk::Media { .. })));
    }

    /// 多个 media segment 连续到达。
    #[test]
    fn multiple_media_segments() {
        let mut p = Fmp4Parser::new();
        let init = synthetic_init(0x42, 0xC0, 0x1E, "avc1", "avcC");
        p.feed(&init);
        let mut stream = Vec::new();
        for i in 0..3 {
            stream.extend_from_slice(&box_("moof", &[i]));
            stream.extend_from_slice(&box_("mdat", &[i; 32]));
        }
        let chunks = p.feed(&stream);
        assert_eq!(chunks.len(), 3, "3 个 moof+mdat = 3 段");
        assert!(chunks.iter().all(|c| matches!(c, Fmp4Chunk::Media { .. })));
    }

    /// HEVC codec 识别（预览切转码的触发条件）。
    #[test]
    fn hevc_detection() {
        let init = synthetic_init(1, 0x60, 0, "hvc1", "hvcC");
        let mut p = Fmp4Parser::new();
        let chunks = p.feed(&init);
        match &chunks[0] {
            Fmp4Chunk::Init { codec, .. } => assert!(codec_is_hevc(codec), "hvc1 必须判为 HEVC: {codec}"),
            _ => panic!("应有 init"),
        }
        assert!(!codec_is_hevc("avc1.42C01E"));
    }
}
