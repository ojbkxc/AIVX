// 设备页：能力渲染 + 空态 + 错误态 + P9-3 添加/删除表单。

import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
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
    // "TP-LINK" 同时出现在厂商行与品牌下拉——用 getAllBy 拿全部匹配
    expect(screen.getAllByText('TP-LINK').length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText('PTZ ✓')).toBeTruthy();
    expect(screen.getByText('事件订阅 —')).toBeTruthy();
  });

  it('空态：提示无设备', async () => {
    render(<DevicesPage api={new MockApiClient([])} />);
    expect(await screen.findByText(/暂无设备/)).toBeTruthy();
  });

  it('P9-3：表单提交走 addDevice 并显示重启提示', async () => {
    const addDevice = vi.fn(async () => ({
      ok: true,
      restart_required: true,
      message: '设备已写入 config.yml，重启服务后生效',
    }));
    const api = new MockApiClient([]);
    api.addDevice = addDevice;
    render(<DevicesPage api={api} />);
    // 填设备 ID（其余字段有默认值：品牌 TP-LINK 模板已可组 URL）
    fireEvent.change(screen.getByTestId('device-id-input'), { target: { value: 'front_door' } });
    fireEvent.click(screen.getByTestId('add-device-submit'));
    await waitFor(() =>
      expect(screen.getByTestId('devices-notice').textContent).toContain('重启'),
    );
    expect(addDevice).toHaveBeenCalledWith(
      expect.objectContaining({ id: 'front_door', record_mode: 'off' }),
    );
  });

  it('P9-3：删除设备走 deleteDevice 并确认', async () => {
    const deleteDevice = vi.fn(async () => ({
      ok: true,
      restart_required: true,
      message: '已从 config.yml 移除',
    }));
    const api = new MockApiClient([dev]);
    api.deleteDevice = deleteDevice;
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    render(<DevicesPage api={api} />);
    fireEvent.click(await screen.findByTestId('delete-cam-1'));
    await waitFor(() => expect(deleteDevice).toHaveBeenCalledWith('cam-1'));
  });
});