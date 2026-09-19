// 设备页：能力渲染 + 空态 + 错误态。

import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { DevicesPage } from './Devices';
import { MockApiClient } from '../api';
import type { Device } from '../types';

const dev: Device = {
  id: 'cam-1',
  name: '后院',
  access_type: 'onvif',
  onvif_url: 'http://1.2.3.4/onvif',
  rtsp_main: 'rtsp://.../stream1',
  rtsp_sub: 'rtsp://.../stream2',
  manufacturer: 'TP-LINK',
  model: null,
  capabilities: {
    ptz: true,
    event_subscription: false,
    imaging: false,
    audio: false,
    main_sub_streams: true,
  },
};

describe('DevicesPage', () => {
  it('渲染设备 + 能力', async () => {
    render(<DevicesPage api={new MockApiClient([dev])} />);
    expect(await screen.findByText('后院')).toBeTruthy();
    expect(screen.getByText('TP-LINK')).toBeTruthy();
    expect(screen.getByText('PTZ ✓')).toBeTruthy();
    expect(screen.getByText('事件订阅 —')).toBeTruthy();
  });

  it('空态：提示无设备', async () => {
    render(<DevicesPage api={new MockApiClient([])} />);
    expect(await screen.findByText(/暂无设备/)).toBeTruthy();
  });
});