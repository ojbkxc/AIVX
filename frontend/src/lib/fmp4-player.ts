// fMP4/MSE 实时预览播放器（P8e，DESIGN.md §8）。
// 协议照 ai-nvr：WS 二进制帧 [0x01]=init（[2B LE codec_len][codec][2B LE audio_len][audio][fMP4]），
// [0x02]=media（[moof+mdat]）。前端 MediaSource 喂给 <video>，catchUpToLive 控制延迟 ~1.2s。

export const FMP4_TYPE_INIT = 0x01;
export const FMP4_TYPE_MEDIA = 0x02;

/** MSE codec 回退表（init 帧给的 codec 失败时依次试）。 */
const CODEC_FALLBACKS = ['avc1.42C01E', 'avc1.4D401F', 'avc1.64001F'];

/** 追赶阈值：延迟超 2s 或超前 0.5s → seek 到最新。 */
const LIVE_SEEK_THRESHOLD = 2.0;
/** prune：播放位置前保留 3s 缓冲。 */
const BUFFER_RETAIN_SECS = 3.0;
/** pending append 队列上限（溢出丢旧保新——直播语义）。 */
const MAX_PENDING = 3;
/** 断流判定：5s 内 videoWidth 仍为 0 视为解码失败。 */
const DECODE_FAIL_MS = 5000;
/** 重连退避：1s 起指数翻倍，封顶 30s。 */
const RECONNECT_CAP_MS = 30000;

export interface Fmp4PlayerState {
  status: 'connecting' | 'playing' | 'failed';
  codec?: string;
  error?: string;
}

/** 解析 init 帧：[0x01][2B LE codec_len][codec][2B LE audio_len][audio][fMP4 data]。 */
export function parseInitFrame(data: ArrayBuffer): { codec: string; audioCodec: string; fmp4: ArrayBuffer } | null {
  const raw = new Uint8Array(data);
  if (raw.length < 1 || raw[0] !== FMP4_TYPE_INIT) return null;
  let off = 1;
  if (raw.length < off + 2) return null;
  const codecLen = raw[off] | (raw[off + 1] << 8);
  off += 2;
  if (raw.length < off + codecLen) return null;
  const codec = asciiDecode(raw.subarray(off, off + codecLen));
  off += codecLen;
  if (raw.length < off + 2) return null;
  const audioLen = raw[off] | (raw[off + 1] << 8);
  off += 2;
  if (raw.length < off + audioLen) return null;
  const audioCodec = asciiDecode(raw.subarray(off, off + audioLen));
  off += audioLen;
  return { codec, audioCodec, fmp4: data.slice(off) };
}

function asciiDecode(bytes: Uint8Array): string {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return s;
}

/** 构造 WS URL：同源 http→ws / https→wss，路径 /api/stream/{id}。 */
export function buildStreamUrl(deviceId: string, loc: Location = window.location): string {
  const proto = loc.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${proto}//${loc.host}/api/stream/${encodeURIComponent(deviceId)}`;
}

/** 附加 fMP4 实时流到 <video>。返回 destroy() 供组件卸载时断开。 */
export function attachFmp4Stream(
  video: HTMLVideoElement,
  url: string,
  onState: (s: Fmp4PlayerState) => void,
): { destroy(): void } {
  let destroyed = false;
  let ws: WebSocket | null = null;
  let mediaSource: MediaSource | null = null;
  let sourceBuffer: SourceBuffer | null = null;
  let appending = false;
  let pending: ArrayBuffer[] = [];
  let pendingInit: ArrayBuffer | null = null;
  let reconnectAttempt = 0;
  let reconnectTimer: number | null = null;
  let decodeCheckTimer: number | null = null;
  let keepUpTimer: number | null = null;

  onState({ status: 'connecting' });

  const scheduleReconnect = (): void => {
    if (destroyed) return;
    const delay = Math.min(1000 * 2 ** reconnectAttempt, RECONNECT_CAP_MS);
    reconnectAttempt += 1;
    reconnectTimer = window.setTimeout(connect, delay);
  };

  const teardownMse = (): void => {
    if (keepUpTimer !== null) window.clearInterval(keepUpTimer);
    keepUpTimer = null;
    if (decodeCheckTimer !== null) window.clearTimeout(decodeCheckTimer);
    decodeCheckTimer = null;
    appending = false;
    pending = [];
    pendingInit = null;
    sourceBuffer = null;
    if (mediaSource !== null) {
      try {
        if (mediaSource.readyState === 'open') mediaSource.endOfStream();
      } catch {
        /* 已关闭则忽略 */
      }
      mediaSource = null;
    }
  };

  const catchUpToLive = (): void => {
    if (sourceBuffer === null || !video.buffered.length) return;
    const end = video.buffered.end(video.buffered.length - 1);
    const delay = end - video.currentTime;
    if (delay > LIVE_SEEK_THRESHOLD || delay < -0.5) {
      video.currentTime = end - 0.05;
    }
  };

  const pruneBuffer = (): void => {
    if (sourceBuffer === null || sourceBuffer.updating || !video.buffered.length) return;
    const start = video.buffered.start(0);
    const keepFrom = video.currentTime - BUFFER_RETAIN_SECS;
    if (keepFrom > start + 0.5) {
      try {
        sourceBuffer.remove(start, keepFrom);
      } catch {
        /* remove 异常不致命 */
      }
    }
  };

  const drainPending = (): void => {
    if (appending || sourceBuffer === null || pending.length === 0) return;
    const data = pending.shift() as ArrayBuffer;
    doAppend(data);
  };

  const doAppend = (data: ArrayBuffer): void => {
    if (sourceBuffer === null) return;
    if (sourceBuffer.updating || appending) {
      queueAppend(data);
      return;
    }
    appending = true;
    try {
      sourceBuffer.appendBuffer(data);
    } catch (e) {
      appending = false;
      if (e instanceof DOMException && e.name === 'QuotaExceededError') {
        try {
          if (video.buffered.length) {
            sourceBuffer.remove(video.buffered.start(0), video.buffered.end(video.buffered.length - 1));
          }
        } catch {
          /* 忽略 */
        }
        pending = [data];
      }
      // 其他异常（解码不兼容等）：放弃本段，直播语义丢段保流
    }
  };

  const queueAppend = (data: ArrayBuffer): void => {
    if (pending.length >= MAX_PENDING) pending.shift();
    pending.push(data);
  };

  const ensureSourceBuffer = (codec: string): boolean => {
    if (sourceBuffer !== null) return true;
    const candidates = [codec, ...CODEC_FALLBACKS.filter((c) => c !== codec)];
    for (const c of candidates) {
      const mime = `video/mp4; codecs="${c}"`;
      if (typeof MediaSource !== 'undefined' && MediaSource.isTypeSupported(mime)) {
        sourceBuffer = mediaSource?.addSourceBuffer(mime) ?? null;
        if (sourceBuffer === null) continue;
        sourceBuffer.mode = 'segments';
        sourceBuffer.addEventListener('updateend', () => {
          appending = false;
          drainPending();
          catchUpToLive();
          pruneBuffer();
        });
        return true;
      }
    }
    return false;
  };

  const handleInit = (data: ArrayBuffer): void => {
    const parsed = parseInitFrame(data);
    if (parsed === null) return;
    teardownMse();
    mediaSource = new MediaSource();
    video.src = URL.createObjectURL(mediaSource);
    mediaSource.addEventListener('sourceopen', () => {
      if (!ensureSourceBuffer(parsed.codec)) {
        onState({ status: 'failed', error: `MSE 不支持 codec ${parsed.codec}` });
        ws?.close();
        return;
      }
      // init 先缓存，等第一个 media 一起 append（消黑闪）
      pendingInit = parsed.fmp4;
      pending = [];
      onState({ status: 'connecting', codec: parsed.codec });
      decodeCheckTimer = window.setTimeout(() => {
        if (video.videoWidth === 0) {
          onState({ status: 'failed', error: '5s 内未出画面' });
          ws?.close();
        }
      }, DECODE_FAIL_MS);
      keepUpTimer = window.setInterval(() => {
        catchUpToLive();
        pruneBuffer();
      }, 500);
    });
  };

  const handleMedia = (data: ArrayBuffer): void => {
    const fmp4 = data.slice(1);
    if (pendingInit !== null) {
      const init = pendingInit;
      pendingInit = null;
      doAppend(init);
      queueAppend(fmp4);
    } else {
      queueAppend(fmp4);
    }
    drainPending();
  };

  const connect = (): void => {
    if (destroyed) return;
    onState({ status: 'connecting' });
    let socket: WebSocket;
    try {
      socket = new WebSocket(url);
    } catch {
      scheduleReconnect();
      return;
    }
    ws = socket;
    socket.binaryType = 'arraybuffer';
    socket.onopen = () => {
      reconnectAttempt = 0;
    };
    socket.onmessage = (ev: MessageEvent) => {
      if (!(ev.data instanceof ArrayBuffer) || ev.data.byteLength < 1) return;
      const tag = new Uint8Array(ev.data, 0, 1)[0];
      if (tag === FMP4_TYPE_INIT) handleInit(ev.data);
      else if (tag === FMP4_TYPE_MEDIA) handleMedia(ev.data);
    };
    socket.onclose = () => {
      if (destroyed) return;
      teardownMse();
      scheduleReconnect();
    };
    socket.onerror = () => {
      socket.close();
    };
  };

  connect();

  return {
    destroy() {
      destroyed = true;
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer);
      reconnectTimer = null;
      teardownMse();
      if (ws !== null) {
        ws.onclose = null;
        ws.close();
        ws = null;
      }
      video.removeAttribute('src');
      try {
        video.load();
      } catch {
        /* jsdom 无媒体栈（仅测试环境触达） */
      }
    },
  };
}
