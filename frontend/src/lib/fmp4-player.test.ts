// fmp4-player 纯函数测试：协议帧解析 + WS URL 构造（MSE/WS 本体部署验证）。

import { describe, expect, it } from 'vitest';
import { buildStreamUrl, parseInitFrame, FMP4_TYPE_INIT } from './fmp4-player';

function frame(parts: number[][]): ArrayBuffer {
  const flat = parts.flat();
  const buf = new ArrayBuffer(flat.length);
  new Uint8Array(buf).set(flat);
  return buf;
}

describe('parseInitFrame', () => {
  it('解析 codec + audio + fMP4 载荷', () => {
    const codec = 'avc1.42C01E';
    const codecBytes = [...codec].map((c) => c.charCodeAt(0));
    const fmp4 = [0x1a, 0x2b, 0x3c, 0x4d];
    const data = frame([
      [FMP4_TYPE_INIT],
      [codecBytes.length & 0xff, (codecBytes.length >> 8) & 0xff],
      codecBytes,
      [0, 0], // audio_len = 0
      fmp4,
    ]);
    const parsed = parseInitFrame(data);
    expect(parsed).not.toBeNull();
    expect(parsed?.codec).toBe('avc1.42C01E');
    expect(parsed?.audioCodec).toBe('');
    expect([...new Uint8Array(parsed!.fmp4)]).toEqual(fmp4);
  });

  it('带 audio codec 的 init 帧', () => {
    const audio = [0x6d, 0x70, 0x34, 0x61]; // "mp4a"
    const data = frame([
      [FMP4_TYPE_INIT],
      [2, 0],
      [0x61, 0x76], // "av"
      [4, 0],
      audio,
      [0xff],
    ]);
    const parsed = parseInitFrame(data);
    expect(parsed?.codec).toBe('av');
    expect(parsed?.audioCodec).toBe('mp4a');
    expect([...new Uint8Array(parsed!.fmp4)]).toEqual([0xff]);
  });

  it('非 init 帧 / 空帧 / 截断帧返回 null', () => {
    expect(parseInitFrame(frame([[0x02]]))).toBeNull();
    expect(parseInitFrame(new ArrayBuffer(0))).toBeNull();
    // codec_len 声明超界
    expect(parseInitFrame(frame([[FMP4_TYPE_INIT], [0xff, 0xff]]))).toBeNull();
  });
});

describe('buildStreamUrl', () => {
  it('http 页面 → ws://', () => {
    const loc = { protocol: 'http:', host: '192.168.31.10:18443' } as Location;
    expect(buildStreamUrl('tp_1-1', loc)).toBe('ws://192.168.31.10:18443/api/stream/tp_1-1');
  });

  it('https 页面 → wss://（公网隧道）', () => {
    const loc = { protocol: 'https:', host: 'gw.example.com:10224' } as Location;
    expect(buildStreamUrl('tp_2-1', loc)).toBe('wss://gw.example.com:10224/api/stream/tp_2-1');
  });

  it('设备 ID 特殊字符 encodeURIComponent', () => {
    const loc = { protocol: 'http:', host: 'h' } as Location;
    expect(buildStreamUrl('a b/c', loc)).toBe('ws://h/api/stream/a%20b%2Fc');
  });
});
