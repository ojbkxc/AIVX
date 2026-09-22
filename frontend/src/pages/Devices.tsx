// 设备管理页（P9-3 升级）：列出摄像头 + 能力渲染（I10）+ 添加设备表单
// （品牌 RTSP 预设模板）+ 删除。写回 config.yml，重启生效（后端提示）。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { Device } from '../types';
import { DeviceCapabilities } from '../components/DeviceCapabilities';

/** 品牌 RTSP URL 预设：占位符 {user}/{pass}/{ip}/{ch}。 */
const BRAND_PRESETS: Record<string, { label: string; url: string; hint: string }> = {
  tplink: {
    label: 'TP-LINK',
    url: 'rtsp://{user}:{pass}@{ip}:554/stream1&channel={ch}',
    hint: 'NVR/IPC 常见：stream1 主码流 + channel 通道号',
  },
  hikvision: {
    label: '海康威视',
    url: 'rtsp://{user}:{pass}@{ip}:554/Streaming/Channels/{ch}01',
    hint: 'IPC：Channels/{ch}01 主码流（102 子码流）',
  },
  dahua: {
    label: '大华',
    url: 'rtsp://{user}:{pass}@{ip}:554/cam/realmonitor?channel={ch}&subtype=0',
    hint: 'subtype=0 主码流（1 子码流）',
  },
  uniview: {
    label: '宇视',
    url: 'rtsp://{user}:{pass}@{ip}:554/media/video1',
    hint: 'video1 主码流（video2 子码流）',
  },
  custom: { label: '自定义', url: '', hint: '直接填完整 RTSP 地址' },
};

const RECORD_OPTIONS: { value: string; label: string }[] = [
  { value: 'off', label: '不录像' },
  { value: 'always', label: '全程录像' },
  { value: 'motion', label: '移动侦测录像' },
];

export function DevicesPage({ api }: { api: ApiClient }): JSX.Element {
  const [devices, setDevices] = useState<Device[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // 表单态
  const [brand, setBrand] = useState('tplink');
  const [id, setId] = useState('');
  const [user, setUser] = useState('admin');
  const [pass, setPass] = useState('');
  const [ip, setIp] = useState('192.168.31.201');
  const [ch, setCh] = useState('1');
  const [customUrl, setCustomUrl] = useState('');
  const [recordMode, setRecordMode] = useState('off');
  const [retainDays, setRetainDays] = useState(7);
  const [busy, setBusy] = useState(false);

  const reload = (): void => {
    setLoading(true);
    api
      .listDevices()
      .then((d) => {
        setDevices(d);
        setLoading(false);
      })
      .catch((e) => {
        setError(String(e));
        setLoading(false);
      });
  };

  useEffect(reload, [api]);

  const buildUrl = (): string => {
    if (brand === 'custom') return customUrl.trim();
    const tpl = BRAND_PRESETS[brand].url;
    return tpl
      .replace('{user}', user)
      .replace('{pass}', pass)
      .replace('{ip}', ip)
      .replace('{ch}', ch);
  };

  const submit = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    const url = buildUrl();
    if (!id.trim() || !url) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const r = await api.addDevice({
        id: id.trim(),
        rtsp_url: url,
        record_mode: recordMode,
        retain_days: recordMode === 'motion' ? retainDays : 0,
      });
      setNotice(r.message || '已写入配置');
      setId('');
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (deviceId: string): Promise<void> => {
    if (!window.confirm(`确认删除设备 ${deviceId}？重启后停拉该路（已录段保留）。`)) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.deleteDevice(deviceId);
      setNotice(r.message || '已移除');
      reload();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="devices" data-testid="devices-page">
      <div className="page-head">
        <h1>设备管理</h1>
      </div>
      {notice && <div className="status" data-testid="devices-notice">{notice}</div>}
      {error && <div className="status error">操作失败：{error}</div>}

      {/* 添加设备表单 */}
      <form className="glass-card add-device-form" onSubmit={submit} data-testid="add-device-form">
        <h2>添加摄像头</h2>
        <div className="form-group">
          <label className="form-hint">品牌预设</label>
          <select
            className="form-input"
            data-testid="brand-select"
            value={brand}
            onChange={(e) => setBrand(e.target.value)}
          >
            {Object.entries(BRAND_PRESETS).map(([key, p]) => (
              <option key={key} value={key}>
                {p.label}
              </option>
            ))}
          </select>
          <div className="form-hint">{BRAND_PRESETS[brand].hint}</div>
        </div>
        <div className="form-row">
          <div className="form-group">
            <label className="form-hint" htmlFor="dev-id">设备 ID（字母数字-_）</label>
            <input
              id="dev-id"
              className="form-input"
              data-testid="device-id-input"
              value={id}
              onChange={(e) => setId(e.target.value)}
              placeholder="如 front_door"
            />
          </div>
          {brand !== 'custom' ? (
            <>
              <div className="form-group">
                <label className="form-hint" htmlFor="dev-ip">IP 地址</label>
                <input
                  id="dev-ip"
                  className="form-input"
                  data-testid="device-ip-input"
                  value={ip}
                  onChange={(e) => setIp(e.target.value)}
                />
              </div>
              <div className="form-group">
                <label className="form-hint" htmlFor="dev-user">用户名</label>
                <input
                  id="dev-user"
                  className="form-input"
                  value={user}
                  onChange={(e) => setUser(e.target.value)}
                />
              </div>
              <div className="form-group">
                <label className="form-hint" htmlFor="dev-pass">密码</label>
                <input
                  id="dev-pass"
                  className="form-input"
                  type="password"
                  value={pass}
                  onChange={(e) => setPass(e.target.value)}
                />
              </div>
              <div className="form-group">
                <label className="form-hint" htmlFor="dev-ch">通道</label>
                <input
                  id="dev-ch"
                  className="form-input"
                  value={ch}
                  onChange={(e) => setCh(e.target.value)}
                />
              </div>
            </>
          ) : (
            <div className="form-group">
              <label className="form-hint" htmlFor="dev-url">完整 RTSP 地址</label>
              <input
                id="dev-url"
                className="form-input"
                data-testid="device-url-input"
                value={customUrl}
                onChange={(e) => setCustomUrl(e.target.value)}
                placeholder="rtsp://user:pass@192.168.1.64:554/..."
              />
            </div>
          )}
        </div>
        <div className="form-row">
          <div className="form-group">
            <label className="form-hint">录像模式</label>
            <select
              className="form-input"
              data-testid="record-mode-select"
              value={recordMode}
              onChange={(e) => setRecordMode(e.target.value)}
            >
              {RECORD_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          </div>
          {recordMode === 'motion' && (
            <div className="form-group">
              <label className="form-hint" htmlFor="dev-retain">保留天数</label>
              <input
                id="dev-retain"
                className="form-input"
                type="number"
                min={1}
                value={retainDays}
                onChange={(e) => setRetainDays(Number(e.target.value))}
              />
            </div>
          )}
        </div>
        {brand !== 'custom' && (
          <div className="form-hint" data-testid="url-preview">
            预览：{buildUrl().replace(pass || '____', '***')}
          </div>
        )}
        <button className="btn btn-primary" type="submit" disabled={busy || !id.trim()} data-testid="add-device-submit">
          {busy ? '写入中…' : '添加设备（重启生效）'}
        </button>
      </form>

      {/* 设备列表 */}
      {loading ? (
        <div className="status">加载中…</div>
      ) : (
        <>
          {devices.length === 0 && <p>暂无设备。用上方表单添加。</p>}
          <ul className="device-list">
            {devices.map((d) => (
              <li key={d.id} className="device-item">
                <div className="device-name">
                  {d.name} <span className="device-id">{d.id}</span>
                </div>
                <div className="device-meta">
                  <span className={`badge ${d.access_type}`}>{d.access_type}</span>
                  <span>{d.manufacturer ?? '未知厂商'}</span>
                </div>
                <DeviceCapabilities caps={d.capabilities} />
                <div className="device-streams">
                  {d.rtsp_main && <span className="stream">主码流 ✓</span>}
                  {d.rtsp_sub && <span className="stream">子码流 ✓</span>}
                  {d.record_mode && d.record_mode !== 'off' && (
                    <span className="badge badge-info">{d.record_mode === 'always' ? '全程录像' : '移动侦测录像'}</span>
                  )}
                </div>
                <button
                  className="btn btn-sm btn-danger"
                  disabled={busy}
                  onClick={() => remove(d.id)}
                  data-testid={`delete-${d.id}`}
                >
                  删除
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
