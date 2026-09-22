// AIGX 式侧边栏（P8d）：分组平铺导航 + 收缩持久化 + 移动端抽屉 + 主题切换。
// 骨架移植自 AIGX components/Sidebar.tsx；裁掉用户区/GlobalSearch/i18n（AIVX 无登录）。

import React from 'react';
import { NavLink, useLocation } from 'react-router-dom';
import {
  MonitorPlay, Siren, PlayCircle, Cctv, ShieldAlert, Bot,
  Menu, PanelLeftClose, PanelLeftOpen, Sun, Moon, Monitor, LogOut,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import MobileDrawer from './MobileDrawer';
import { cycleTheme, getThemeMode } from '../lib/theme';
import type { ThemeMode } from '../lib/theme';

interface NavItem {
  path: string;
  label: string;
  icon: LucideIcon;
  end?: boolean;
}

interface NavGroup {
  key: string;
  label: string;
  items: NavItem[];
}

// 分组平铺（AIGX navGroups 同款：短分组标签 + 组内平铺菜单，无折叠交互）
const navGroups: NavGroup[] = [
  {
    key: 'watch',
    label: '监看',
    items: [
      { path: '/live', label: '实时预览', icon: MonitorPlay, end: true },
      { path: '/alarms', label: '报警中心', icon: Siren },
      { path: '/recordings', label: '录像回放', icon: PlayCircle },
    ],
  },
  {
    key: 'manage',
    label: '管理',
    items: [
      { path: '/devices', label: '设备管理', icon: Cctv },
      { path: '/rules', label: '布控配置', icon: ShieldAlert },
      { path: '/agent', label: 'Agent 运维', icon: Bot },
    ],
  },
];

const THEME_META: Record<ThemeMode, { icon: LucideIcon; label: string }> = {
  light: { icon: Sun, label: '主题：浅色' },
  dark: { icon: Moon, label: '主题：深色' },
  system: { icon: Monitor, label: '主题：跟随系统' },
};

interface SidebarContentProps {
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** 移动端抽屉形态：隐藏收缩按钮（抽屉本身即开合） */
  isDrawer: boolean;
  /** 登出回调（鉴权未启用时不渲染登出按钮） */
  authEnabled: boolean;
  onLogout?: () => void;
}

/** 侧栏内容（桌面常驻 / 移动抽屉共用） */
function SidebarContent({ collapsed, onToggleCollapsed, isDrawer, authEnabled, onLogout }: SidebarContentProps): JSX.Element {
  const [theme, setTheme] = React.useState<ThemeMode>(getThemeMode());

  const toggleTheme = (): void => {
    setTheme(cycleTheme());
  };

  const ThemeIcon = THEME_META[theme].icon;

  return (
    <aside
      className={`sidebar-aside ${collapsed ? 'sidebar-aside-collapsed' : ''} ${isDrawer ? 'sidebar-aside-drawer' : ''}`}
    >
      {/* 品牌：accent 实底方块 + 标题（AIGX sidebar-head 同款） */}
      <div className="sidebar-head">
        <div className="sidebar-logo">
          <Cctv size={15} strokeWidth={1.8} />
        </div>
        <div className="sidebar-head-text">
          <div className="sidebar-title">AIVX</div>
          <div className="sidebar-subtitle">AI Video eXtended</div>
        </div>
      </div>

      {/* Nav：展开态分组平铺；收缩态仅图标 + title tooltip */}
      <nav className="sidebar-nav">
        {navGroups.map((group) => (
          <div key={group.key} className="sidebar-group">
            <div className="sidebar-group-label" title={group.label}>
              {group.label}
            </div>
            {group.items.map((item) => (
              <NavLink
                key={item.path}
                to={item.path}
                end={item.end}
                className={({ isActive }) => `nav-item ${isActive ? 'active' : ''}`}
                title={item.label}
              >
                <item.icon size={15} strokeWidth={1.8} />
                <span>{item.label}</span>
              </NavLink>
            ))}
          </div>
        ))}
      </nav>

      {/* 唯一收缩开关：nav 与 footer 之间的固定行，整行（含提示文字）可点击 */}
      {!isDrawer && (
        <button
          type="button"
          className="sidebar-collapse-row"
          onClick={onToggleCollapsed}
          title={collapsed ? '展开侧边栏' : '收起侧边栏'}
          aria-label={collapsed ? '展开侧边栏' : '收起侧边栏'}
        >
          <span className="sidebar-collapse-btn">
            {collapsed ? <PanelLeftOpen size={15} /> : <PanelLeftClose size={15} />}
          </span>
          {!collapsed && <span className="sidebar-collapse-hint">收起侧边栏</span>}
        </button>
      )}

      {/* Footer：主题三态切换行 + 登出（鉴权启用时） */}
      <div className="sidebar-footer">
        <button
          type="button"
          className="theme-toggle-btn"
          onClick={toggleTheme}
          title={THEME_META[theme].label}
        >
          <ThemeIcon size={15} strokeWidth={1.8} />
          <span>{THEME_META[theme].label}</span>
        </button>
        {authEnabled && (
          <button
            type="button"
            className="theme-toggle-btn"
            onClick={onLogout}
            title="退出登录"
            data-testid="logout-btn"
          >
            <LogOut size={15} strokeWidth={1.8} />
            <span>退出登录</span>
          </button>
        )}
      </div>
    </aside>
  );
}

export function Sidebar({ authEnabled = false, onLogout }: { authEnabled?: boolean; onLogout?: () => void }): JSX.Element {
  const location = useLocation();
  // 移动端抽屉开关：仅 ≤768px 由汉堡按钮触发
  const [mobileOpen, setMobileOpen] = React.useState<boolean>(false);
  // 侧边栏展开/收缩：唯一开关，收缩后为窄图标栏
  const [collapsed, setCollapsed] = React.useState<boolean>(() => {
    try {
      return localStorage.getItem('sidebar_collapsed_global') === 'true';
    } catch {
      return false;
    }
  });

  // 切换展开/收缩并持久化（全局布局联动 .main-content 边距）
  const toggleCollapsed = (): void => {
    setCollapsed((prev) => {
      const next = !prev;
      try { localStorage.setItem('sidebar_collapsed_global', next ? 'true' : 'false'); } catch { /* 忽略持久化失败 */ }
      return next;
    });
  };

  // 同步 data 属性（刷新后保持收缩态；随 collapsed 变化保持全局布局一致）
  React.useEffect(() => {
    document.documentElement.dataset.sidebarCollapsed = collapsed ? 'true' : 'false';
  }, [collapsed]);

  // 路由切换后自动收起移动端抽屉
  React.useEffect(() => {
    setMobileOpen(false);
  }, [location.pathname]);

  return (
    <>
      {/* 移动端汉堡按钮 */}
      <button
        type="button"
        className="mobile-menu-btn"
        onClick={() => setMobileOpen(true)}
        aria-label="打开菜单"
      >
        <Menu size={18} strokeWidth={2} />
      </button>
      {/* 桌面端常驻侧栏（≤768px 由 CSS 隐藏） */}
      <div className="sidebar-desktop">
        <SidebarContent collapsed={collapsed} onToggleCollapsed={toggleCollapsed} isDrawer={false} authEnabled={authEnabled} onLogout={onLogout} />
      </div>
      {/* 移动端抽屉（恒为展开形态） */}
      <MobileDrawer open={mobileOpen} onClose={() => setMobileOpen(false)} ariaLabel="导航菜单">
        <SidebarContent collapsed={false} onToggleCollapsed={toggleCollapsed} isDrawer authEnabled={authEnabled} onLogout={onLogout} />
      </MobileDrawer>
    </>
  );
}
