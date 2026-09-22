// 登录页测试（P9-1）：错误提示 + 成功回调。

import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { LoginPage } from './Login';
import type { ApiClient } from '../api';

function makeApi(overrides: Partial<ApiClient> = {}): ApiClient {
  return {
    listDevices: vi.fn(async () => []),
    healthz: vi.fn(async () => ({ status: 'ok', max_seq: 0, version: 'mock' })),
    alarmsSummary: vi.fn(async () => ({ active: 0, projected_events: 0 })),
    listRecordings: vi.fn(async () => []),
    listConfig: vi.fn(async () => []),
    authStatus: vi.fn(async () => ({ auth_enabled: true, logged_in: false })),
    login: vi.fn(async () => ({ ok: true, auth_enabled: true })),
    logout: vi.fn(async () => ({ ok: true })),
    ...overrides,
  } as unknown as ApiClient;
}

describe('LoginPage', () => {
  it('密码错误显示错误提示', async () => {
    const api = makeApi({
      login: vi.fn(async () => {
        throw new Error('HTTP 401');
      }),
    });
    render(<LoginPage api={api} onLoggedIn={() => {}} />);
    fireEvent.change(screen.getByTestId('login-password'), { target: { value: 'wrong' } });
    fireEvent.click(screen.getByTestId('login-submit'));
    await waitFor(() => expect(screen.getByTestId('login-error')).toBeTruthy());
  });

  it('登录成功触发 onLoggedIn', async () => {
    const api = makeApi();
    const onLoggedIn = vi.fn();
    render(<LoginPage api={api} onLoggedIn={onLoggedIn} />);
    fireEvent.change(screen.getByTestId('login-password'), { target: { value: 'pass' } });
    fireEvent.click(screen.getByTestId('login-submit'));
    await waitFor(() => expect(onLoggedIn).toHaveBeenCalled());
  });
});
