// I10 机器验证：DeviceCapabilities 按能力数据渲染，不支持显示"—"而非报错。

import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { DeviceCapabilities } from './DeviceCapabilities';
import type { Capabilities } from '../types';

const full: Capabilities = {
  ptz: true,
  event_subscription: true,
  imaging: true,
  audio: false,
  main_sub_streams: true,
};

describe('DeviceCapabilities (I10)', () => {
  it('支持的能力显示 ✓', () => {
    render(<DeviceCapabilities caps={full} />);
    expect(screen.getByText('PTZ ✓')).toBeTruthy();
    expect(screen.getByText('事件订阅 ✓')).toBeTruthy();
    expect(screen.getByText('成像 ✓')).toBeTruthy();
    expect(screen.getByText('主/子码流 ✓')).toBeTruthy();
  });

  it('不支持的能力显示 —（数据不是异常）', () => {
    render(<DeviceCapabilities caps={full} />);
    expect(screen.getByText('音频 —')).toBeTruthy();
  });

  it('全不支持 → 全部显示 —（不抛错）', () => {
    const none: Capabilities = {
      ptz: false,
      event_subscription: false,
      imaging: false,
      audio: false,
      main_sub_streams: false,
    };
    render(<DeviceCapabilities caps={none} />);
    const el = screen.getByTestId('device-capabilities');
    expect(el.textContent).toContain('PTZ —');
    // 不抛错，正常渲染
    expect(el).toBeTruthy();
  });
});