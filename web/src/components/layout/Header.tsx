import { useLocation } from 'react-router-dom';
import { t } from '@/lib/i18n';

const routeTitles: Record<string, string> = {
  '/': 'nav.dashboard',
  '/agent': 'nav.agent',
  '/tools': 'nav.tools',
  '/cron': 'nav.cron',
  '/integrations': 'nav.integrations',
  '/memory': 'nav.memory',
  '/config': 'nav.config',
  '/models': 'nav.cost',
  '/logs': 'nav.logs',
  '/doctor': 'nav.doctor',
};

export default function Header() {
  const location = useLocation();
  const titleKey = routeTitles[location.pathname] ?? 'nav.dashboard';

  return (
    <header className="h-14 bg-gray-800 border-b border-gray-700 flex items-center justify-between px-6">
      <h1 className="text-lg font-semibold text-white">{t(titleKey)}</h1>
      <span className="text-xs font-medium uppercase tracking-[0.24em] text-gray-500">
        local ollama runtime
      </span>
    </header>
  );
}
