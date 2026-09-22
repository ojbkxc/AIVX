// P9-1 鉴权门：启动探测 auth/status → 未登录只渲染登录页。
// 应用层包裹（非路由守卫）：后端未启用鉴权时零开销直通。
// render-prop：children(authEnabled, onLogout)——Sidebar 用它渲染登出按钮。

import { ReactNode, useEffect, useState } from 'react';
import type { ApiClient } from '../api';
import { LoginPage } from '../pages/Login';

type GateState =
  | { phase: 'checking' }
  | { phase: 'login' }
  | { phase: 'ready'; authEnabled: boolean };

export function AuthGate({ api, children }: { api: ApiClient; children: (authEnabled: boolean, onLogout: () => void) => ReactNode }): JSX.Element {
  const [state, setState] = useState<GateState>({ phase: 'checking' });

  const probe = (): void => {
    api
      .authStatus()
      .then((s) => {
        if (s.auth_enabled && !s.logged_in) {
          setState({ phase: 'login' });
        } else {
          setState({ phase: 'ready', authEnabled: s.auth_enabled });
        }
      })
      .catch(() => setState({ phase: 'ready', authEnabled: false })); // 后端不可达：放行（页面自会显示错误）
  };

  const logout = (): void => {
    api.logout().finally(probe); // 撤销 session 后重新探测 → 回登录页
  };

  useEffect(probe, [api]);

  if (state.phase === 'checking') {
    return <div className="login-page"><div className="status">连接中…</div></div>;
  }
  if (state.phase === 'login') {
    return <LoginPage api={api} onLoggedIn={probe} />;
  }
  return <>{children(state.authEnabled, logout)}</>;
}
