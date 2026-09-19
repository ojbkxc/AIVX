// 报警中心页测试：汇总卡片。

import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { AlarmsPage } from './Alarms';
import { MockApiClient } from '../api';

describe('AlarmsPage', () => {
  it('渲染汇总卡片', async () => {
    render(<AlarmsPage api={new MockApiClient()} />);
    expect(await screen.findByTestId('active-alarms')).toBeTruthy();
    expect(await screen.findByText('已投影事件')).toBeTruthy();
  });
});
