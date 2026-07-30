import { useEffect, useRef, useState } from 'react';
import { NavLink } from 'react-router-dom';
import {
  Activity,
  Brain,
  Network,
  BookText,
  FileText,
  FolderOpen,
  LayoutDashboard,
  MessageSquare,
  Plug,
  ScrollText,
  Stethoscope,
  Wrench,
  Clock,
  Settings,
  Database,
  PanelLeftClose,
  PanelLeftOpen,
  X,
  type LucideIcon,
} from 'lucide-react';
import { useLocale } from '@/lib/i18n';
import { navigationItems, type NavigationIcon } from '@/lib/navigation';

const navigationIcons: Record<NavigationIcon, LucideIcon> = {
  dashboard: LayoutDashboard,
  agent: MessageSquare,
  runs: Activity,
  federation: Network,
  tools: Wrench,
  cron: Clock,
  integrations: Plug,
  memory: Brain,
  database: Database,
  workspace: FolderOpen,
  files: FileText,
  prompts: BookText,
  logs: ScrollText,
  doctor: Stethoscope,
  config: Settings,
};

interface SidebarProps {
  collapsed: boolean;
  mobileOpen: boolean;
  onMobileClose: () => void;
  onNavigate: () => void;
  onToggle: () => void;
}

export default function Sidebar({
  collapsed,
  mobileOpen,
  onMobileClose,
  onNavigate,
  onToggle,
}: SidebarProps) {
  const { t } = useLocale();
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const sidebarRef = useRef<HTMLElement>(null);
  const [desktopNavigation, setDesktopNavigation] = useState(() =>
    window.matchMedia('(min-width: 768px)').matches,
  );

  useEffect(() => {
    const desktopQuery = window.matchMedia('(min-width: 768px)');
    const updateDesktopNavigation = () => setDesktopNavigation(desktopQuery.matches);

    updateDesktopNavigation();
    desktopQuery.addEventListener('change', updateDesktopNavigation);
    return () => desktopQuery.removeEventListener('change', updateDesktopNavigation);
  }, []);

  useEffect(() => {
    if (!mobileOpen) return;

    closeButtonRef.current?.focus();
    const handleDrawerKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        onMobileClose();
        return;
      }

      if (event.key !== 'Tab' || !sidebarRef.current) return;
      const focusable = Array.from(
        sidebarRef.current.querySelectorAll<HTMLElement>('a[href], button:not([disabled])'),
      ).filter((element) => element.getClientRects().length > 0 && element.tabIndex >= 0);
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;

      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', handleDrawerKeyDown);
    return () => document.removeEventListener('keydown', handleDrawerKeyDown);
  }, [mobileOpen, onMobileClose]);

  return (
    <>
      <button
        type="button"
        aria-label={t('nav.close_menu')}
        className={`fixed inset-0 z-30 bg-black/65 transition-opacity md:hidden ${
          mobileOpen ? 'pointer-events-auto opacity-100' : 'pointer-events-none opacity-0'
        }`}
        onClick={onMobileClose}
        tabIndex={mobileOpen ? 0 : -1}
      />

      <aside
        ref={sidebarRef}
        id="primary-navigation"
        aria-label={t('nav.primary')}
        aria-hidden={!desktopNavigation && !mobileOpen}
        inert={!desktopNavigation && !mobileOpen ? true : undefined}
        className={`fixed left-0 top-0 z-40 flex h-dvh w-[min(18rem,calc(100vw-3rem))] flex-col border-r border-gray-800 bg-gray-900 transition-[transform,width] duration-200 md:translate-x-0 ${
          collapsed ? 'md:w-14' : 'md:w-60'
        } ${mobileOpen ? 'translate-x-0' : '-translate-x-full'}`}
      >
        <div className="flex min-h-[60px] items-center justify-between border-b border-gray-800 px-3 py-2">
          <span
            className={`truncate text-lg font-semibold tracking-wide text-white ${
              collapsed ? 'md:hidden' : ''
            }`}
          >
            LlamaFarm
          </span>
          <button
            ref={closeButtonRef}
            type="button"
            onClick={onMobileClose}
            className="inline-flex min-h-11 min-w-11 items-center justify-center rounded-lg text-gray-400 transition-colors hover:bg-gray-800 hover:text-white md:hidden"
            aria-label={t('nav.close_menu')}
          >
            <X className="h-5 w-5" aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={onToggle}
            className={`hidden min-h-9 min-w-9 flex-shrink-0 items-center justify-center rounded-lg text-gray-400 transition-colors hover:bg-gray-800 hover:text-white md:inline-flex ${
              collapsed ? 'mx-auto' : ''
            }`}
            title={collapsed ? t('nav.expand') : t('nav.collapse')}
            aria-label={collapsed ? t('nav.expand') : t('nav.collapse')}
          >
            {collapsed ? (
              <PanelLeftOpen className="h-4 w-4" aria-hidden="true" />
            ) : (
              <PanelLeftClose className="h-4 w-4" aria-hidden="true" />
            )}
          </button>
        </div>

        <nav className="flex-1 space-y-1 overflow-y-auto px-2 py-3">
          {navigationItems.map(({ to, icon, labelKey }) => {
            const Icon = navigationIcons[icon];
            return (
              <NavLink
                key={to}
                to={to}
                end
                onClick={onNavigate}
                title={collapsed ? t(labelKey) : undefined}
                className={({ isActive }) =>
                  [
                    'flex min-h-11 items-center gap-3 rounded-lg px-3 py-2.5 text-sm font-medium transition-colors',
                    collapsed ? 'md:justify-center md:px-2' : '',
                    isActive
                      ? 'bg-blue-600 text-white'
                      : 'text-gray-300 hover:bg-gray-800 hover:text-white',
                  ].join(' ')
                }
              >
                <Icon className="h-5 w-5 flex-shrink-0" aria-hidden="true" />
                <span className={collapsed ? 'md:hidden' : ''}>{t(labelKey)}</span>
              </NavLink>
            );
          })}
        </nav>
      </aside>
    </>
  );
}
