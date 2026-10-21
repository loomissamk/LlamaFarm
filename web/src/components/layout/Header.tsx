import { useLocation } from 'react-router-dom';
import { Menu } from 'lucide-react';
import { t } from '@/lib/i18n';

const routeTitles: Record<string, string> = {
  '/': 'nav.dashboard',
  '/agent': 'nav.agent',
  '/federation': 'nav.federation',
  '/memory': 'nav.memory',
  '/tools': 'nav.tools',
  '/cron': 'nav.cron',
  '/runs': 'nav.runs',
  '/database': 'nav.database',
  '/workspace': 'nav.workspace',
  '/workspace/files': 'nav.workspace',
  '/workspace/prompts': 'nav.prompts',
  '/logs': 'nav.logs',
  '/doctor': 'nav.doctor',
  '/config': 'nav.config',
};

interface HeaderProps {
  onMenuClick: () => void;
}

export default function Header({ onMenuClick }: HeaderProps) {
  const location = useLocation();
  const titleKey = routeTitles[location.pathname] ?? 'nav.dashboard';

  return (
    <header className="h-14 bg-gray-800 border-b border-gray-700 flex items-center justify-between gap-3 px-3 sm:px-6">
      <div className="flex min-w-0 items-center gap-2">
        <button
          onClick={onMenuClick}
          className="flex-shrink-0 rounded-lg p-1.5 text-gray-400 transition-colors hover:bg-gray-700 hover:text-white md:hidden"
          title="Open menu"
          aria-label="Open menu"
        >
          <Menu className="h-5 w-5" />
        </button>
        <h1 className="truncate text-lg font-semibold text-white">{t(titleKey)}</h1>
      </div>
      <span className="hidden text-xs font-medium uppercase tracking-[0.24em] text-gray-500 sm:inline">
        local llamafarm runtime
      </span>
    </header>
  );
}
