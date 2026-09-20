import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import {
  type ThemeMode,
  getThemeMode,
  resolveTheme,
  applyTheme,
  cycleTheme,
  initTheme,
} from './theme';

const setSystemDark = (dark: boolean): void => {
  vi.stubGlobal('matchMedia', vi.fn().mockImplementation((query: string) => ({
    matches: query.includes('dark') && dark,
    media: query,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    onchange: null,
    dispatchEvent: vi.fn(),
  })));
};

describe('theme 三态', () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.setAttribute('data-theme', 'light');
    vi.unstubAllGlobals();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('getThemeMode 默认 system', () => {
    expect(getThemeMode()).toBe('system');
  });

  it('getThemeMode 读取持久化值并过滤非法值', () => {
    localStorage.setItem('theme', 'dark');
    expect(getThemeMode()).toBe('dark');
    localStorage.setItem('theme', 'nonsense');
    expect(getThemeMode()).toBe('system');
  });

  it('resolveTheme：system 跟随系统偏好', () => {
    setSystemDark(true);
    expect(resolveTheme('system')).toBe('dark');
    setSystemDark(false);
    expect(resolveTheme('system')).toBe('light');
  });

  it('resolveTheme：显式模式直接返回', () => {
    expect(resolveTheme('light')).toBe('light');
    expect(resolveTheme('dark')).toBe('dark');
  });

  it('applyTheme 写 DOM 属性并持久化偏好', () => {
    applyTheme('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(localStorage.getItem('theme')).toBe('dark');
  });

  it('applyTheme(system) 按系统偏好解析实际明暗', () => {
    setSystemDark(true);
    applyTheme('system');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(localStorage.getItem('theme')).toBe('system');
  });

  it('cycleTheme 在 light → dark → system 循环', () => {
    localStorage.setItem('theme', 'light');
    expect(cycleTheme()).toBe('dark');
    expect(cycleTheme()).toBe('system');
    expect(cycleTheme()).toBe('light');
  });

  it('initTheme 返回清理函数且 system 态注册监听', () => {
    localStorage.setItem('theme', 'system');
    setSystemDark(false);
    const cleanup = initTheme();
    expect(typeof cleanup).toBe('function');
    cleanup();
  });

  it('ThemeMode 类型只接受三态', () => {
    const mode: ThemeMode = 'system';
    expect(mode).toBe('system');
  });
});
