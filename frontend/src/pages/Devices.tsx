// 设备管理页：列出摄像头 + 能力渲染（I10）。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { Device } from '../types';
import { DeviceCapabilities } from '../components/DeviceCapabilities';

export function DevicesPage({ api }: { api: ApiClient }) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
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
  }, [api]);

  if (loading) return <div className="status">加载中…</div>;
  if (error) return <div className="status error">加载失败：{error}</div>;

  return (
    <div className="devices" data-testid="devices-page">
      <div className="page-head">
        <h1>设备管理</h1>
      </div>
      {devices.length === 0 && <p>暂无设备。点击"发现"扫描 ONVIF 摄像头。</p>}
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
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}