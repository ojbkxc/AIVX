// 布控配置页（P8d骨架）：规则引擎的画布编辑器随 P8e 接入。
// 当前展示规则引擎模型说明 + 设备布控状态。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { Device } from '../types';

const RULE_MODEL = `// 规则模型（DESIGN.md §9，数据驱动）
Rule {
  when: Condition 树（And/Or/Not + TrackInZone/LabelIs/Dwell/CrossedLine/SpeedGt/CountGt/TimeIn）
  window: Option<Sliding>   // 滑动窗口聚合
  actions: [Notify | Snapshot | Record | VerifyWithLlm]
  cooldown: Duration
}`;

export function RulesPage({ api }: { api: ApiClient }) {
  const [devices, setDevices] = useState<Device[]>([]);

  useEffect(() => {
    api.listDevices().then(setDevices).catch(() => {});
  }, [api]);

  return (
    <div className="rules-page" data-testid="rules-page">
      <div className="page-head">
        <h1>布控配置</h1>
      </div>
      <div className="rules-model">
        <pre>{RULE_MODEL}</pre>
      </div>
      <p className="hint">
        区域/警戒线画布编辑器随 P8e 接入（canvas 多边形 + 越线方向）。
      </p>
      <ul className="rules-device-list">
        {devices.map((d) => (
          <li key={d.id}>
            {d.name} — PTZ {d.capabilities.ptz ? '✓' : '✗'}
          </li>
        ))}
      </ul>
    </div>
  );
}
