// 报警中心页（P8d）：活跃报警 + 事件流计数（alarms summary 5s 轮询）。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { AlarmsSummary as Summary } from '../types';

export function AlarmsPage({ api }: { api: ApiClient }) {
  const [summary, setSummary] = useState<Summary | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let stop = false;
    const tick = async () => {
      try {
        const s = await api.alarmsSummary();
        if (!stop) {
          setSummary(s);
          setError(null);
        }
      } catch (e) {
        if (!stop) setError(String(e));
      }
    };
    tick();
    const t = setInterval(tick, 5000);
    return () => {
      stop = true;
      clearInterval(t);
    };
  }, [api]);

  return (
    <div className="alarms-page" data-testid="alarms-page">
      <div className="page-head">
        <h1>报警中心</h1>
      </div>
      {error && <div className="status error">后端不可达：{error}</div>}
      <div className="alarm-cards">
        <div className="stat-card">
          <span className="stat-value" data-testid="active-alarms">
            {summary?.active ?? '—'}
          </span>
          <span className="stat-label">活跃报警</span>
        </div>
        <div className="stat-card">
          <span className="stat-value" data-testid="projected-events">
            {summary?.projected_events ?? '—'}
          </span>
          <span className="stat-label">已投影事件</span>
        </div>
      </div>
      <p className="hint">
        报警明细列表随投影器派生表 API（P8e）接入后展示；当前为汇总视图。
      </p>
    </div>
  );
}
