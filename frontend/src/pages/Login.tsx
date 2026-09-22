// 登录页（P9-1）：单用户口令登录。鉴权未启用时不会路由到此页
// （AuthGate 探测 auth_enabled=false 直接放行）。

import { useState } from 'react';
import type { ApiClient } from '../api';

export function LoginPage({ api, onLoggedIn }: { api: ApiClient; onLoggedIn: () => void }): JSX.Element {
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    if (!password || busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.login(password);
      onLoggedIn();
    } catch {
      setError('密码错误或服务不可用');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="login-page" data-testid="login-page">
      <form className="glass-card login-card" onSubmit={submit}>
        <div className="sidebar-head login-brand">
          <div className="sidebar-logo">AIVX</div>
          <div className="sidebar-head-text">
            <div className="sidebar-title">AIVX</div>
            <div className="sidebar-subtitle">AI Video eXtended</div>
          </div>
        </div>
        <h1>登录</h1>
        <div className="form-group">
          <label className="form-hint" htmlFor="login-password">口令</label>
          <input
            id="login-password"
            className="form-input"
            type="password"
            data-testid="login-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoFocus
          />
        </div>
        {error && <p className="status error" data-testid="login-error">{error}</p>}
        <button className="btn btn-primary" type="submit" disabled={busy || !password} data-testid="login-submit">
          {busy ? '登录中…' : '登录'}
        </button>
      </form>
    </div>
  );
}
