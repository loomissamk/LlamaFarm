import { NavLink } from 'react-router-dom';
import {
  Network,
  BookText,
  FolderOpen,
  LayoutDashboard,
  MessageSquare,
  Wrench,
  Clock,
  Puzzle,
  Brain,
  Settings,
  Activity,
  ClipboardList,
  Stethoscope,
  Database,
  PanelLeftClose,
  PanelLeftOpen,
} from 'lucide-react';
import { t } from '@/lib/i18n';

const navItems = [
  { to: '/', icon: LayoutDashboard, labelKey: 'nav.dashboard' },
  { to: '/agent', icon: MessageSquare, labelKey: 'nav.agent' },
  { to: '/federation', icon: Network, labelKey: 'nav.federation' },
  { to: '/tools', icon: Wrench, labelKey: 'nav.tools' },
  { to: '/cron', icon: Clock, labelKey: 'nav.cron' },
  { to: '/integrations', icon: Puzzle, labelKey: 'nav.integrations' },
  { to: '/memory', icon: Brain, labelKey: 'nav.memory' },
  { to: '/database', icon: Database, labelKey: 'nav.database' },
  { to: '/workspace', icon: FolderOpen, labelKey: 'nav.workspace' },
  { to: '/workspace/prompts', icon: BookText, labelKey: 'nav.prompts' },
  { to: '/config', icon: Settings, labelKey: 'nav.config' },
  { to: '/runs', icon: ClipboardList, labelKey: 'nav.runs' },
  { to: '/logs', icon: Activity, labelKey: 'nav.logs' },
  { to: '/doctor', icon: Stethoscope, labelKey: 'nav.doctor' },
];

interface SidebarProps {
  collapsed: boolean;
  onToggle: () => void;
}

export default function Sidebar({ collapsed, onToggle }: SidebarProps) {
  return (
    <aside
      className={`fixed top-0 left-0 h-screen bg-gray-900 flex flex-col border-r border-gray-800 transition-[width] duration-200 z-30 ${
        collapsed ? 'w-14' : 'w-60'
      }`}
    >
      {/* Logo / Title */}
      <div className="px-3 py-4 border-b border-gray-800 flex items-center justify-between min-h-[60px]">
        {!collapsed && (
          <span className="text-lg font-semibold text-white tracking-wide truncate">
            LlamaFarm
          </span>
        )}
        <button
          onClick={onToggle}
          className={`flex-shrink-0 rounded-lg p-1.5 text-gray-400 transition-colors hover:bg-gray-800 hover:text-white ${collapsed ? 'mx-auto' : ''}`}
          title={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
        >
          {collapsed ? (
            <PanelLeftOpen className="h-4 w-4" />
          ) : (
            <PanelLeftClose className="h-4 w-4" />
          )}
        </button>
      </div>

      {/* Navigation */}
      <nav className="flex-1 overflow-y-auto py-4 px-2 space-y-1">
        {navItems.map(({ to, icon: Icon, labelKey }) => (
          <NavLink
            key={to}
            to={to}
            end
            title={collapsed ? t(labelKey) : undefined}
            className={({ isActive }) =>
              [
                'flex items-center rounded-lg text-sm font-medium transition-colors',
                collapsed ? 'justify-center px-2 py-2.5' : 'gap-3 px-3 py-2.5',
                isActive
                  ? 'bg-blue-600 text-white'
                  : 'text-gray-300 hover:bg-gray-800 hover:text-white',
              ].join(' ')
            }
          >
            <Icon className="h-5 w-5 flex-shrink-0" />
            {!collapsed && <span>{t(labelKey)}</span>}
          </NavLink>
        ))}
      </nav>
    </aside>
  );
}
