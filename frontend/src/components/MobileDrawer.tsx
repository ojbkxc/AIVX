// 移植自 AIGX components/ui/MobileDrawer.tsx（玻璃拟态抽屉）。
// cn() 是 clsx+tailwind-merge 封装；AIVX 无 Tailwind，此处用模板串合并。

import React, { useEffect } from 'react';

export interface MobileDrawerProps {
  open: boolean;
  onClose: () => void;
  children: React.ReactNode;
  /** 抽屉宽度（CSS 值），默认 216px 与 --sidebar-width 一致 */
  width?: string;
  /** 可访问性标签 */
  ariaLabel?: string;
  className?: string;
}

/**
 * MobileDrawer — 移动端抽屉容器（玻璃拟态）。
 * 仅在 open 时渲染；遮罩点击/Esc 关闭；背景滚动锁定。
 * 桌面端由 Sidebar 常驻，此组件服务于 ≤768px 的汉堡菜单场景。
 */
export default function MobileDrawer({
  open,
  onClose,
  children,
  width,
  ariaLabel,
  className
}: MobileDrawerProps) {
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    const prev = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      window.removeEventListener('keydown', onKey);
      document.body.style.overflow = prev;
    };
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div className="mobile-drawer-overlay" onClick={onClose} aria-hidden="true">
      <aside
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel}
        className={className ? `mobile-drawer ${className}` : 'mobile-drawer'}
        style={width ? { width } : undefined}
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </aside>
    </div>
  );
}
