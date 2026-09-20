// 录像回放页（P8e）：按设备列出录像段（recordings 派生表）+ 点击回放。
// 回放 = <video src="/recordings/{device}/{file}">（ServeDir + Range/seek 免费）。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { Device, Recording } from '../types';

/** file_path 是服务器绝对路径（如 /opt/aivx/data/record/tp_1-2/seg_x.mp4），
 * 回放 URL 取设备目录之后的相对部分。 */
function playbackUrl(r: Recording): string | null {
  const marker = `/record/${r.device_id}/`;
  const idx = r.file_path.indexOf(marker);
  if (idx < 0) return null;
  const rel = r.file_path.slice(idx + marker.length);
  return `/recordings/${encodeURIComponent(r.device_id)}/${encodeURIComponent(rel)}`;
}

export function RecordingsPage({ api }: { api: ApiClient }) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [recordings, setRecordings] = useState<Recording[]>([]);
  const [playing, setPlaying] = useState<Recording | null>(null);

  useEffect(() => {
    api.listDevices().then((d) => {
      setDevices(d);
      const firstRec = d.find((x) => x.record_mode !== 'off') ?? d[0];
      if (firstRec) setSelected(firstRec.id);
    });
  }, [api]);

  useEffect(() => {
    if (selected) {
      setPlaying(null);
      api.listRecordings(selected).then(setRecordings);
    }
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
          {playing && (
            <div className="rec-player">
              <video controls autoPlay src={playbackUrl(playing) ?? undefined} />
              <button className="btn" onClick={() => setPlaying(null)}>
                关闭回放
              </button>
            </div>
          )}
          {recordings.length === 0 && (
            <p className="hint">该设备暂无录像段（录像 10 分钟一段，段索引 10s 刷新）。</p>
          )}
          <ul>
            {recordings
              .slice()
              .reverse()
              .map((r) => (
                <li key={r.id} className="rec-item">
                  <span>{new Date(r.start_ts * 1000).toLocaleString()}</span>
                  <span>{Math.round(r.duration_secs)}s</span>
                  <button className="btn btn-sm" onClick={() => setPlaying(r)}>
                    回放
                  </button>
                </li>
              ))}
          </ul>
        </main>
      </div>
    </div>
  );
}
