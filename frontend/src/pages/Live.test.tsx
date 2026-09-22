// 实时预览页测试：流状态渲染 + 分辨率占位 + PTZ 面板（P9-5）。

import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { LivePage } from './Live';
import { MockApiClient } from '../api';
import type { Device } from '../types';

const cams: Device[] = [
  { id: 'tp_1-1', name: '固定相机1', access_type: 'rtsp', onvif_url: null, rtsp_main: 'r', rtsp_sub: 'r', manufacturer: 'TP-LINK', model: null, capabilities: { ptz: false, event_subscription: false, imaging: false, audio: false, main_sub_streams: true }, record_mode: 'off' },
  { id: 'tp_1-2', name: '移动相机1', access_type: 'rtsp', onvif_url: null, rtsp_main: 'r', rtsp_sub: 'r', manufacturer: 'TP-LINK', model: null, capabilities: { ptz: true, event_subscription: false, imaging: false, audio: false, main_sub_streams: true }, record_mode: 'always' },
];

describe('LivePage', () => {
  it('渲染每路卡片 + 正常状态', async () => {
    render(<LivePage api={new MockApiClient(cams)} />);
    expect(await screen.findByText('固定相机1')).toBeTruthy();
    expect(screen.getByText('移动相机1')).toBeTruthy();
    // healthz mock 的 streams 状态 ok（多路卡都会渲染"正常"——查第一路即可）
    expect(await screen.findAllByText('正常')).not.toHaveLength(0);
  });

  it('录像模式标记', async () => {
    render(<LivePage api={new MockApiClient(cams)} />);
    expect(await screen.findByText(/REC always/)).toBeTruthy();
  });

  it('PTZ：capabilities.ptz 设备渲染面板，点击→ 走 ptzMove', async () => {
    const api = new MockApiClient(cams);
    const ptzMove = vi.fn(api.ptzMove.bind(api));
    api.ptzMove = ptzMove;
    render(<LivePage api={api} />);
    // ptz 能力设备有面板；无能力设备没有
    expect(await screen.findByTestId('ptz-tp_1-2')).toBeTruthy();
    expect(screen.queryByTestId('ptz-tp_1-1')).toBeNull();
    fireEvent.click(screen.getByTestId('ptz-right'));
    await waitFor(() => expect(ptzMove).toHaveBeenCalledWith('tp_1-2', { d_pan: 0.3, d_tilt: 0 }));
  });

  it('PTZ：保存预置位走 ptzPreset(set) 并显示 id', async () => {
    const api = new MockApiClient(cams);
    const ptzPreset = vi.fn(api.ptzPreset.bind(api));
    api.ptzPreset = ptzPreset;
    render(<LivePage api={api} />);
    fireEvent.change(await screen.findByTestId('ptz-preset-name'), { target: { value: '门口' } });
    fireEvent.click(screen.getByTestId('ptz-save'));
    await waitFor(() => expect(ptzPreset).toHaveBeenCalledWith('tp_1-2', { type: 'set', name: '门口' }));
    expect(await screen.findByText(/预置位已保存（id=9）/)).toBeTruthy();
  });
});
