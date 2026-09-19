// 录像回放页（P8d骨架）：按设备列出录像段。
// 段索引 API（recordings 投影器）随 P8e 接入——当前空态。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { Device, Recording } from '../types';

export function RecordingsPage({ api }: { api: ApiClient }) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [recordings, setRecordings] = useState<Recording[]>([]);

  useEffect(() => {
    api.listDevices().then((d) => {
      setDevices(d);
      if (d.length > 0) setSelected(d[0].id);
    });
  }, [api]);

  useEffect(() => {
    if (selected) api.listRecordings(selected).then(setRecordings);
  }, [api, selected]);

  return (
    <div className="recordings-page" data-testid="recordings-page">
      <div className="page-head">
        <h1>录像回放</h1>
      </div>
      <div className="rec-layout">
        <aside className="rec-device-list">
          {devices.map((d) => (
            <button
              key={d.id}
              className={`rec-device ${selected === d.id ? 'active' : ''} ${
                d.record_mode === 'off' ? 'no-rec' : ''
              }`}
              onClick={() => setSelected(d.id)}
            >
              {d.name}
              <span className="rec-mode">{d.record_mode ?? 'off'}</span>
            </button>
          ))}
        </aside>
        <main className="rec-list">
          {recordings.length === 0 && (
            <p className="hint">该设备暂无录像索引（录像段 API 随 P8e 接入）。</p>
          )}
          <ul>
            {recordings.map((r) => (
              <li key={r.id} className="rec-item">
                <span>{new Date(r.start_ts * 1000).toLocaleString()}</span>
                <span>{Math.round(r.duration_secs)}s</span>
              </li>
            ))}
          </ul>
        </main>
      </div>
    </div>
  );
}
