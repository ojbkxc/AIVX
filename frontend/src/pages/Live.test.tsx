// 实时预览页测试：流状态渲染 + 分辨率占位。

import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
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
});
