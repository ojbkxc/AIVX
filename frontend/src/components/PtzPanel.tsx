// PTZ 云台操控面板（P9-5）：方向增量移动 + 位置回显 + 预置位 set/goto。
// 挂在 Live 页 stream-card 下（capabilities.ptz 的设备才渲染——后端
// /api/ptz 对不支持设备 404，前端用 devices 列表的 capabilities 过滤）。

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ApiClient, PtzStatus } from '../api';

/** 每次方向点击的增量（度）。实测此云台 pan≈±1.x°/tilt≈[-1,0.5]，
 * 大步进会直接顶到物理极限——0.3° 一档微调 + 长按连点。 */
const STEP = 0.3;

export function PtzPanel({ api, deviceId }: { api: ApiClient; deviceId: string }) {
  const [status, setStatus] = useState<PtzStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [presetName, setPresetName] = useState('');
  const [gotoId, setGotoId] = useState('');
  const [notice, setNotice] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setInterval> | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await api.ptzStatus(deviceId));
      setErr(null);
    } catch (e) {
      setErr(String(e));
    }
  }, [api, deviceId]);

  // 位置回显：5s 轮询（moving 时值得看进度；idle 时低频保新鲜）。
  useEffect(() => {
    refresh();
    timer.current = setInterval(refresh, 5000);
    return () => {
      if (timer.current) clearInterval(timer.current);
    };
  }, [refresh]);

  const move = async (dPan: number, dTilt: number) => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      const st = await api.ptzMove(deviceId, { d_pan: dPan, d_tilt: dTilt });
      setStatus(st);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const savePreset = async () => {
    if (!presetName.trim()) return;
    setBusy(true);
    setErr(null);
    setNotice(null);
    try {
      const r = await api.ptzPreset(deviceId, { type: 'set', name: presetName.trim() });
      setNotice(`预置位已保存（id=${r.id}）`);
      setPresetName('');
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const jumpPreset = async () => {
    const id = Number(gotoId);
    if (!gotoId.trim() || Number.isNaN(id)) return;
    setBusy(true);
    setErr(null);
    setNotice(null);
    try {
      await api.ptzPreset(deviceId, { type: 'goto', id });
      setNotice(`已跳转预置位 ${id}`);
      refresh();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="ptz-panel" data-testid={`ptz-${deviceId}`}>
      <div className="ptz-pad" aria-label="云台方向控制">
        <span /> {/* 占位（左上空） */}
        <button className="ptz-btn" data-testid="ptz-up" disabled={busy} onClick={() => move(0, STEP)}>
          ↑
        </button>
        <span />
        <button className="ptz-btn" data-testid="ptz-left" disabled={busy} onClick={() => move(-STEP, 0)}>
          ←
        </button>
        <button className="ptz-btn" data-testid="ptz-home" disabled={busy} onClick={() => api.ptzMove(deviceId, { pan: 0, tilt: 0 }).then(setStatus).catch((e) => setErr(String(e)))}>
          ⌂
        </button>
        <button className="ptz-btn" data-testid="ptz-right" disabled={busy} onClick={() => move(STEP, 0)}>
          →
        </button>
        <span />
        <button className="ptz-btn" data-testid="ptz-down" disabled={busy} onClick={() => move(0, -STEP)}>
          ↓
        </button>
        <span />
      </div>
      <div className="ptz-pos" data-testid="ptz-pos">
        {status
          ? `${status.position_pan.toFixed(2)}° / ${status.position_tilt.toFixed(2)}°${status.moving ? '（转动中）' : ''}`
          : '位置读取中…'}
      </div>
      <div className="ptz-presets">
        <input
          className="ptz-input"
          data-testid="ptz-preset-name"
          placeholder="预置位名"
          value={presetName}
          onChange={(e) => setPresetName(e.target.value)}
        />
        <button className="btn btn-sm" data-testid="ptz-save" disabled={busy || !presetName.trim()} onClick={savePreset}>
          保存
        </button>
        <input
          className="ptz-input"
          data-testid="ptz-goto-id"
          placeholder="id"
          value={gotoId}
          onChange={(e) => setGotoId(e.target.value)}
        />
        <button className="btn btn-sm" data-testid="ptz-goto" disabled={busy || !gotoId.trim()} onClick={jumpPreset}>
          跳转
        </button>
      </div>
      {notice && <p className="ptz-notice">{notice}</p>}
      {err && <p className="ptz-err">{err}</p>}
    </div>
  );
}
