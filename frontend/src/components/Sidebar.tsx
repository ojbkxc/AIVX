// AIGX 式侧边栏（P8d）：五页导航 + 品牌区。

import { NavLink } from 'react-router-dom';

const ITEMS = [
  { to: '/live', label: '实时预览', icon: '▶' },
  { to: '/devices', label: '设备管理', icon: '⌗' },
  { to: '/rules', label: '布控配置', icon: '◇' },
  { to: '/alarms', label: '报警中心', icon: '⚠' },
  { to: '/recordings', label: '录像回放', icon: '⏵' },
  { to: '/agent', label: 'Agent 运维', icon: '✦' },
];

export function Sidebar() {
  return (
    <nav className="sidebar" data-testid="sidebar">
      <div className="sidebar-brand">
        <span className="brand-mark">AIVX</span>
        <span className="brand-sub">AI Video eXtended</span>
      </div>
      <ul className="sidebar-menu">
        {ITEMS.map((item) => (
          <li key={item.to}>
            <NavLink
              to={item.to}
              className={({ isActive }) => (isActive ? 'active' : '')}
            >
              <span className="icon">{item.icon}</span>
              <span>{item.label}</span>
            </NavLink>
          </li>
        ))}
      </ul>
    </nav>
  );
}
