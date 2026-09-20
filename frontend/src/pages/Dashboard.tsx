// 布控配置页（P8e 只读降级）：每路设备的接入地址（RTSP 脱敏）/
// 录像模式/流状态。画布编辑器（区域/警戒线）随后续阶段接入。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { SourceConfig } from '../types';

const RECORD_LABEL: Record<string, string> = {
  off: '关闭',
  always: '全程录像',
  motion: '移动侦测',
};

export function RulesPage({ api }: { api: ApiClient }) {
  const [configs, setConfigs] = useState<SourceConfig[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .listConfig()
      .then((c) => setConfigs(c))
      .catch((e) => setError(String(e)));
  }, [api]);

  return (
    <div className="rules-page" data-testid="rules-page">
      <div className="page-head">
        <h1>布控配置</h1>
      </div>
      <p className="hint">
        只读视图：接入地址已脱敏（凭据不外泄）。区域/警戒线画布编辑器随后续阶段接入。
      </p>
      {error && <p className="status error">配置读取失败：{error}</p>}
      {configs.length === 0 && !error && <p className="hint">暂无配置的摄像头（data/config.yml）。</p>}
      <div className="devices device-list">
        {configs.map((c) => (
          <div key={c.id} className="device-item">
            <div className="device-name">
              {c.name}
              <span className="badge">{RECORD_LABEL[c.record_mode] ?? c.record_mode}</span>
            </div>
            <div className="device-id">{c.rtsp_main ?? '（无主码流）'}</div>
            <div className="capabilities">
              <span className="badge">流状态：{c.state}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
