// 实时预览页（P8d）：4 路画面位 + 流状态（healthz 2s 轮询）。
// 布局参照 AIGX Dashboard——卡片网格 + 实时指标。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { Device, StreamStatus } from '../types';

const STATE_LABEL: Record<StreamStatus['state'], string> = {
  ok: '正常',
  connecting: '连接中',
  reconnecting: '重连中',
  degraded: '降级',
  stopped: '已停止',
};

function StreamCard({ device, status }: { device: Device; status?: StreamStatus }) {
  const state = status?.state ?? 'connecting';
  const fps = status ? Math.round(status.decode_frames / 30) : 0; // 30s 采样窗粗估
  return (
    <div className={`stream-card state-${state}`} data-testid={`stream-${device.id}`}>
      <div className="stream-view">
        {/* P8e 接视频播放器（fMP4/MSE）；当前显示状态占位 */}
        <div className="stream-placeholder">
          <span className="stream-name">{device.name}</span>
        </div>
      </div>
      <div className="stream-meta">
        <span className={`state-badge ${state}`}>{STATE_LABEL[state]}</span>
        {status && (
          <>
            <span>{status.decode_frames} 帧</span>
            <span>{status.inferences} 次推理</span>
            <span>~{fps} fps</span>
          </>
        )}
        {device.record_mode && device.record_mode !== 'off' && (
          <span className="rec-badge">REC {device.record_mode}</span>
        )}
      </div>
    </div>
  );
}

export function LivePage({ api }: { api: ApiClient }) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [streams, setStreams] = useState<Record<string, StreamStatus>>({});
  const [version, setVersion] = useState('');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.listDevices().then(setDevices).catch((e) => setError(String(e)));
  }, [api]);

  useEffect(() => {
    let stop = false;
    const tick = async () => {
      try {
        const h = await api.healthz();
        if (stop) return;
        setVersion(h.version);
        const map: Record<string, StreamStatus> = {};
        for (const s of h.streams ?? []) map[s.id] = s;
        setStreams(map);
        setError(null);
      } catch (e) {
        if (!stop) setError(String(e));
      }
    };
    tick();
    const t = setInterval(tick, 2000);
    return () => {
      stop = true;
      clearInterval(t);
    };
  }, [api]);

  if (error) return <div className="status error">后端不可达：{error}</div>;

  return (
    <div className="live-page" data-testid="live-page">
      <div className="page-head">
        <h1>实时预览</h1>
        <span className="version-tag">v{version}</span>
      </div>
      <div className="stream-grid">
        {devices.map((d) => (
          <StreamCard key={d.id} device={d} status={streams[d.id]} />
        ))}
      </div>
    </div>
  );
}
