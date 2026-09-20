/**
 * AIGX 主题系统 — 参照 open-webui 的 theme store：
 * system / light / dark 三态，CSS 变量由 data-theme 属性驱动，
 * system 态跟随操作系统的 prefers-color-scheme（含运行时切换）。
 */

export type ThemeMode = 'system' | 'light' | 'dark';

const STORAGE_KEY = 'theme';
const DARK_QUERY = '(prefers-color-scheme: dark)';

/** 读取持久化的主题偏好（非法值回退 system） */
export function getThemeMode(): ThemeMode {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === 'light' || saved === 'dark' || saved === 'system') return saved;
  } catch {
    // localStorage 不可用时回退默认
  }
  return 'system';
}

/** system 态下解析为实际明暗值 */
export function resolveTheme(mode: ThemeMode): 'light' | 'dark' {
  if (mode === 'light' || mode === 'dark') return mode;
  if (typeof window !== 'undefined' && window.matchMedia) {
    return window.matchMedia(DARK_QUERY).matches ? 'dark' : 'light';
  }
  return 'light';
}

/** 应用主题到 DOM（data-theme 属性是 CSS 变量的唯一驱动） */
export function applyTheme(mode: ThemeMode): void {
  const resolved = resolveTheme(mode);
  document.documentElement.setAttribute('data-theme', resolved);
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // 忽略持久化失败
  }
}

/** 初始化主题（main.tsx 首屏调用，避免 FOUC） */
export function initTheme(): () => void {
  const mode = getThemeMode();
  applyTheme(mode);
  if (mode !== 'system' || !window.matchMedia) {
    return () => undefined;
  }
  const query = window.matchMedia(DARK_QUERY);
  const onChange = (): void => {
    applyTheme('system');
  };
  if (typeof query.addEventListener === 'function') {
    query.addEventListener('change', onChange);
    return () => query.removeEventListener('change', onChange);
  }
  query.addListener(onChange);
  return () => query.removeListener(onChange);
}

/** 在 light → dark → system 之间循环，供登录页/侧边栏切换按钮使用 */
export function cycleTheme(): ThemeMode {
  const order: ThemeMode[] = ['light', 'dark', 'system'];
  const current = getThemeMode();
  const next = order[(order.indexOf(current) + 1) % order.length];
  applyTheme(next);
  return next;
}
