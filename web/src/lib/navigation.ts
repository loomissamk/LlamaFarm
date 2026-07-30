export type NavigationIcon =
  | 'dashboard'
  | 'agent'
  | 'runs'
  | 'federation'
  | 'tools'
  | 'cron'
  | 'integrations'
  | 'memory'
  | 'database'
  | 'workspace'
  | 'files'
  | 'prompts'
  | 'logs'
  | 'doctor'
  | 'config';

export interface NavigationItem {
  to: string;
  labelKey: string;
  icon: NavigationIcon;
}

/**
 * The canonical list of pages operators can navigate to directly.
 *
 * Redirect-only compatibility routes such as /models and /cost intentionally
 * stay out of this list.
 */
export const navigationItems: readonly NavigationItem[] = [
  { to: '/', labelKey: 'nav.dashboard', icon: 'dashboard' },
  { to: '/agent', labelKey: 'nav.agent', icon: 'agent' },
  { to: '/runs', labelKey: 'nav.runs', icon: 'runs' },
  { to: '/federation', labelKey: 'nav.federation', icon: 'federation' },
  { to: '/tools', labelKey: 'nav.tools', icon: 'tools' },
  { to: '/cron', labelKey: 'nav.cron', icon: 'cron' },
  { to: '/integrations', labelKey: 'nav.integrations', icon: 'integrations' },
  { to: '/memory', labelKey: 'nav.memory', icon: 'memory' },
  { to: '/database', labelKey: 'nav.database', icon: 'database' },
  { to: '/workspace', labelKey: 'nav.workspace', icon: 'workspace' },
  { to: '/workspace/files', labelKey: 'nav.files', icon: 'files' },
  { to: '/workspace/prompts', labelKey: 'nav.prompts', icon: 'prompts' },
  { to: '/logs', labelKey: 'nav.logs', icon: 'logs' },
  { to: '/doctor', labelKey: 'nav.doctor', icon: 'doctor' },
  { to: '/config', labelKey: 'nav.config', icon: 'config' },
];

const routeTitleKeys = new Map(
  navigationItems.map(({ to, labelKey }) => [to, labelKey]),
);

export function titleKeyForPath(pathname: string): string {
  return routeTitleKeys.get(pathname) ?? 'not_found.title';
}
