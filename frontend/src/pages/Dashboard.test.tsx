// 布控配置页测试（P9-4）：行渲染 + 保存走 updateDevice + 类别勾选。

import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { RulesPage } from './Dashboard';
import { MockApiClient } from '../api';
import type { SourceConfig } from '../types';

const cfg: SourceConfig = {
  id: 'tp_1-1',
  name: '固定相机',
  rtsp_main: 'rtsp://***@192.168.31.201/stream1',
  rtsp_sub: null,
  record_mode: 'motion',
  retain_days: 7,
  classes: ['person'],
  state: 'ok',
};

describe('RulesPage', () => {
  it('渲染配置行（类别 + 录像模式）', async () => {
    render(<RulesPage api={new MockApiClient([], undefined, undefined, undefined, [cfg])} />);
    expect(await screen.findByText('固定相机')).toBeTruthy();
    expect(screen.getByTestId('chip-person').className).toContain('chip-on');
  });

  it('保存：勾选 dog 后 PUT classes=[person,dog]', async () => {
    const updateDevice = vi.fn(async () => ({
      ok: true,
      restart_required: false,
      message: '已生效并写回 config.yml',
    }));
    const api = new MockApiClient([], undefined, undefined, undefined, [cfg]);
    api.updateDevice = updateDevice;
    render(<RulesPage api={api} />);
    fireEvent.click(await screen.findByTestId('chip-dog'));
    fireEvent.click(screen.getByTestId('save-tp_1-1'));
    await waitFor(() => expect(updateDevice).toHaveBeenCalled());
    const calls = updateDevice.mock.calls as unknown as [string, { classes: string[]; record_mode: string }][];
    const req = calls[0][1];
    expect([...req.classes].sort()).toEqual(['dog', 'person']);
    expect(req.record_mode).toBe('motion');
  });
});
