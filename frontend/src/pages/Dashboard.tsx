// 布控配置页（P9-4 可编辑升级）：每路设备的录像模式/保留天数/检测类别。
// 保存 = PUT /api/devices/{id} → 后端写回 config.yml + 运行时字段即时生效；
// 类别过滤需重启（T2 启动期参数）——后端 restart_required 标志驱动提示。

import { useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import type { SourceConfig } from '../types';

const RECORD_LABEL: Record<string, string> = {
  off: '关闭',
  always: '全程录像',
  motion: '移动侦测',
};

/** 常用 COCO 类别快捷勾选（全部 80 类不铺——高频子集 + 自由输入） */
const QUICK_CLASSES = ['person', 'cat', 'dog', 'bird', 'car'];

/** 单行设备编辑状态。 */
interface RowEdit {
  record_mode: string;
  retain_days: number;
  classes: string;
  /** 自由输入的额外类别（逗号分隔） */
  extra: string;
}

function toEdit(c: SourceConfig): RowEdit {
  const quick = new Set(QUICK_CLASSES);
  const known = c.classes.filter((x) => quick.has(x));
  const extra = c.classes.filter((x) => !quick.has(x));
  return {
    record_mode: c.record_mode,
    retain_days: c.retain_days || 7,
    classes: known.join(','),
    extra: extra.join(','),
  };
}

function editClasses(e: RowEdit): string[] {
  const set = new Set<string>([
    ...e.classes.split(',').filter(Boolean),
    ...e.extra.split(',').map((s) => s.trim()).filter(Boolean),
  ]);
  return [...set];
}

function DeviceConfigRow({
  api,
  cfg,
  onNotice,
}: {
  api: ApiClient;
  cfg: SourceConfig;
  onNotice: (msg: string) => void;
}): JSX.Element {
  const [edit, setEdit] = useState<RowEdit>(toEdit(cfg));
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const save = async (): Promise<void> => {
    setBusy(true);
    setErr(null);
    try {
      const r = await api.updateDevice(cfg.id, {
        record_mode: edit.record_mode,
        retain_days: edit.record_mode === 'motion' ? edit.retain_days : 0,
        classes: editClasses(edit),
      });
      onNotice(r.restart_required ? `${cfg.id}：${r.message}` : `${cfg.id}：${r.message}`);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const toggleQuick = (name: string): void => {
    const set = new Set(edit.classes.split(',').filter(Boolean));
    if (set.has(name)) set.delete(name);
    else set.add(name);
    setEdit({ ...edit, classes: [...set].join(',') });
  };

  const quickSet = new Set(edit.classes.split(',').filter(Boolean));

  return (
    <div className="device-item" data-testid={`config-row-${cfg.id}`}>
      <div className="device-name">{cfg.name || cfg.id}</div>
      <div className="device-id">{cfg.rtsp_main ?? '（无主码流）'}</div>
      <div className="capabilities">
        <span className="badge">流状态：{cfg.state}</span>
      </div>

      <div className="form-row config-edit-row">
        <div className="form-group">
          <label className="form-hint">录像模式</label>
          <select
            className="form-input"
            value={edit.record_mode}
            onChange={(e) => setEdit({ ...edit, record_mode: e.target.value })}
          >
            {Object.entries(RECORD_LABEL).map(([v, label]) => (
              <option key={v} value={v}>
                {label}
              </option>
            ))}
          </select>
        </div>
        {edit.record_mode === 'motion' && (
          <div className="form-group">
            <label className="form-hint">保留天数</label>
            <input
              className="form-input"
              type="number"
              min={1}
              value={edit.retain_days}
              onChange={(e) => setEdit({ ...edit, retain_days: Number(e.target.value) })}
            />
          </div>
        )}
      </div>

      <div className="form-group">
        <label className="form-hint">检测类别（空 = 全部；改动需重启生效）</label>
        <div className="class-chips">
          {QUICK_CLASSES.map((name) => (
            <button
              key={name}
              type="button"
              className={`badge chip ${quickSet.has(name) ? 'chip-on' : ''}`}
              data-testid={`chip-${name}`}
              onClick={() => toggleQuick(name)}
            >
              {name}
            </button>
          ))}
        </div>
        <input
          className="form-input"
          placeholder="其他类别，逗号分隔（如 horse, sheep）"
          value={edit.extra}
          onChange={(e) => setEdit({ ...edit, extra: e.target.value })}
        />
      </div>

      {err && <p className="status error">保存失败：{err}</p>}
      <button className="btn btn-primary btn-sm" disabled={busy} onClick={save} data-testid={`save-${cfg.id}`}>
        {busy ? '保存中…' : '保存'}
      </button>
    </div>
  );
}

export function RulesPage({ api }: { api: ApiClient }): JSX.Element {
  const [configs, setConfigs] = useState<SourceConfig[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

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
        录像模式/保留天数保存即生效并写回 config.yml；检测类别是分析线程启动参数，改动后重启服务生效。
        区域/警戒线画布编辑器随后续阶段接入。
      </p>
      {notice && <div className="status" data-testid="rules-notice">{notice}</div>}
      {error && <p className="status error">配置读取失败：{error}</p>}
      {configs.length === 0 && !error && <p className="hint">暂无配置的摄像头（先在设备管理页添加）。</p>}
      <div className="devices device-list">
        {configs.map((c) => (
          <DeviceConfigRow key={c.id} api={api} cfg={c} onNotice={setNotice} />
        ))}
      </div>
    </div>
  );
}
