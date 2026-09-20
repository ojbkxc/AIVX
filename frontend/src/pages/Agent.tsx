// Agent 运维工作台（P8e）：对话式查报警/诊断。LLM 走 AIGX 网关渠道
//（后端 env AIVX_LLM_URL/KEY/MODEL；未配置时 StubProvider 桩对话）。

import { useRef, useState } from 'react';

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

/** 后端 AgentEvent JSON（POST /api/agent/chat 响应 events[]）。 */
interface AgentEventJson {
  type: 'thinking' | 'tool_call' | 'tool_result' | 'approval_request' | 'approval_resolved' | 'final' | 'error';
  turn?: number;
  name?: string;
  arguments?: string;
  ok?: boolean;
  text?: string;
  content?: string;
  message?: string;
}

/** 聊天流里渲染的一条（user 或 agent 轮的过程聚合）。 */
interface ChatItem {
  role: 'user' | 'assistant';
  text: string;
  /** 工具调用/结果小行（agent 轮内嵌渲染）。 */
  steps?: { name: string; ok?: boolean; text?: string }[];
}

function renderEvents(events: AgentEventJson[]): { text: string; steps: ChatItem['steps'] } {
  const steps: NonNullable<ChatItem['steps']> = [];
  let finalText = '（本轮未产生答复）';
  for (const e of events) {
    if (e.type === 'tool_call') {
      steps.push({ name: `${e.name}(${e.arguments ?? ''})` });
    } else if (e.type === 'tool_result') {
      const last = steps[steps.length - 1];
      if (last) {
        last.ok = e.ok;
        last.text = e.text;
      }
    } else if (e.type === 'final' && e.content) {
      finalText = e.content;
    } else if (e.type === 'error') {
      finalText = `出错：${e.message ?? ''}`;
    }
  }
  return { text: finalText, steps };
}

export function AgentPage() {
  const [messages, setMessages] = useState<ChatItem[]>([
    {
      role: 'assistant',
      text: 'AIVX Agent 就绪。可自然语言查设备/报警/录像/流健康（观察员只读角色）。',
    },
  ]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [llmHint, setLlmHint] = useState<string | null>(null);
  const chatRef = useRef<HTMLDivElement>(null);

  const send = async (): Promise<void> => {
    const text = input.trim();
    if (!text || busy) return;
    setBusy(true);
    setInput('');
    setMessages((prev) => [...prev, { role: 'user', text }]);
    try {
      const resp = await fetch('/api/agent/chat', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ message: text }),
      });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const data = (await resp.json()) as { events: AgentEventJson[]; llm_configured: boolean };
      const { text: finalText, steps } = renderEvents(data.events);
      setLlmHint(data.llm_configured ? null : 'LLM 未配置（AIVX_LLM_URL/KEY）——当前为桩对话，仅演示工具链路。');
      setMessages((prev) => [...prev, { role: 'assistant', text: finalText, steps }]);
    } catch (e) {
      setMessages((prev) => [...prev, { role: 'assistant', text: `请求失败：${String(e)}` }]);
    } finally {
      setBusy(false);
      requestAnimationFrame(() => {
        chatRef.current?.scrollTo({ top: chatRef.current.scrollHeight });
      });
    }
  };

  return (
    <div className="agent-page" data-testid="agent-page">
      <div className="page-head">
        <h1>Agent 运维</h1>
      </div>
      <div className="agent-layout">
        <div className="agent-chat" ref={chatRef}>
          {messages.map((m, i) => (
            <div key={i} className={`msg ${m.role}`}>
              <div>{m.text}</div>
              {m.steps && m.steps.length > 0 && (
                <div className="agent-steps">
                  {m.steps.map((s, j) => (
                    <div key={j} className={`agent-step ${s.ok === false ? 'step-fail' : ''}`}>
                      <code>{s.name}</code>
                      {s.text && <span>{s.text}</span>}
                    </div>
                  ))}
                </div>
              )}
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
      {llmHint && <p className="hint">{llmHint}</p>}
      <div className="agent-input-row">
        <input
          className="glass-input"
          value={input}
          placeholder="例如：查一下最近的报警"
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') void send();
          }}
          disabled={busy}
        />
        <button className="btn btn-primary" onClick={() => void send()} disabled={busy || !input.trim()}>
          {busy ? '推理中…' : '发送'}
        </button>
      </div>
    </div>
  );
}
