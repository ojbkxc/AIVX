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

/** 时长 600 → "10:00"（段列表可读化；小时段 "1:02:03"）。 */
function fmtDur(sec: number): string {
  const s = Math.max(0, Math.round(sec));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  const mm = h > 0 ? String(m).padStart(2, '0') : String(m);
  return h > 0 ? `${h}:${mm}:${String(r).padStart(2, '0')}` : `${mm}:${String(r).padStart(2, '0')}`;
}

const PAGE = 50;

export function RecordingsPage({ api }: { api: ApiClient }) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [recordings, setRecordings] = useState<Recording[]>([]);
  const [playing, setPlaying] = useState<Recording | null>(null);
  const [limit, setLimit] = useState(PAGE);

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
      setLimit(PAGE);
      api.listRecordings(selected).then(setRecordings);
    }
  }, [api, selected]);

  // 倒序（最新在前）+ 分页截断（700+ 段全量渲染卡顿）。
  const shown = recordings.slice().reverse().slice(0, limit);

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
          <ul data-testid="rec-list">
            {shown.map((r) => (
              <li key={r.id} className="rec-item">
                <span>{new Date(r.start_ts * 1000).toLocaleString()}</span>
                <span>{fmtDur(r.duration_secs)}</span>
                <button className="btn btn-sm" onClick={() => setPlaying(r)}>
                  回放
                </button>
              </li>
            ))}
          </ul>
          {limit < recordings.length && (
            <button className="btn" data-testid="rec-more" onClick={() => setLimit(limit + PAGE)}>
              加载更多（{limit}/{recordings.length}）
            </button>
          )}
        </main>
      </div>
    </div>
  );
}
