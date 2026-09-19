// Agent 运维工作台（P8d骨架）：对话式查报警/诊断（AIGX Agent.tsx 风格）。
// LLM 后端随 P8e 接入（AIGX 网关渠道）；当前是 UI 壳 + 工具清单。

import { useState } from 'react';

const TOOLS = [
  { name: 'nvr_list_devices', risk: '只读', desc: '列出摄像头 + 在线状态' },
  { name: 'nvr_list_alarms', risk: '只读', desc: '按时间/设备查报警' },
  { name: 'nvr_search_recording', risk: '只读', desc: '查回放片段' },
  { name: 'nvr_diagnostics', risk: '只读', desc: '流健康/FPS/CPU 体检' },
  { name: 'nvr_snapshot', risk: '低危', desc: '立即抓拍' },
  { name: 'nvr_set_zone', risk: '低危', desc: '改布控（审计留痕）' },
  { name: 'nvr_delete_recording', risk: '高危', desc: '删录像（审批矩阵）' },
];

const RISK_COLOR: Record<string, string> = {
  只读: 'risk-readonly',
  低危: 'risk-low',
  高危: 'risk-high',
};

export function AgentPage() {
  const [messages] = useState<{ role: 'user' | 'assistant'; text: string }[]>([
    { role: 'assistant', text: 'AIVX Agent 就绪。推理后端（AIGX 网关）随 P8e 接入——当前可浏览工具白名单。' },
  ]);

  return (
    <div className="agent-page" data-testid="agent-page">
      <div className="page-head">
        <h1>Agent 运维</h1>
      </div>
      <div className="agent-layout">
        <div className="agent-chat">
          {messages.map((m, i) => (
            <div key={i} className={`msg ${m.role}`}>
              {m.text}
            </div>
          ))}
        </div>
        <aside className="agent-tools">
          <h3>工具白名单（三层风险）</h3>
          <ul>
            {TOOLS.map((t) => (
              <li key={t.name}>
                <span className={`risk ${RISK_COLOR[t.risk]}`}>{t.risk}</span>
                <code>{t.name}</code>
                <p>{t.desc}</p>
              </li>
            ))}
          </ul>
        </aside>
      </div>
    </div>
  );
}
