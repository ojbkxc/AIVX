// 报警中心页（P8e）：汇总卡 + 明细表（alarms summary 5s 轮询）。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { Alarm, AlarmsSummary as Summary } from '../types';

function fmtTs(ts: number): string {
  return new Date(ts * 1000).toLocaleString();
}

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

  const items: Alarm[] = summary?.items ?? [];

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
      {items.length === 0 ? (
        <p className="hint">暂无报警记录。</p>
      ) : (
        <div className="glass-card">
        <table data-testid="alarm-table">
          <thead>
            <tr>
              <th>时间</th>
              <th>设备</th>
              <th>规则</th>
              <th>目标</th>
              <th>置信度</th>
              <th>状态</th>
            </tr>
          </thead>
          <tbody>
            {items.map((a) => (
              <tr key={a.alarm_id}>
                <td>{fmtTs(a.raised_ts)}</td>
                <td>{a.device_id}</td>
                <td>{a.rule_id}</td>
                <td>{a.label ?? '—'}</td>
                <td>{a.score != null ? a.score.toFixed(2) : '—'}</td>
                <td>
                  {a.cleared_ts ? (
                    <span className="badge badge-neutral">已清除</span>
                  ) : (
                    <span className="badge badge-danger">活跃</span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        </div>
      )}
    </div>
  );
}
