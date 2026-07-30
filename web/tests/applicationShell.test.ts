import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { navigationItems, titleKeyForPath } from '../src/lib/navigation.ts';

const readSource = (path: string) =>
  readFileSync(fileURLToPath(new URL(path, import.meta.url)), 'utf8');

test('navigation exposes every intentional application page', () => {
  assert.deepEqual(
    navigationItems.map(({ to }) => to),
    [
      '/',
      '/agent',
      '/runs',
      '/federation',
      '/tools',
      '/cron',
      '/integrations',
      '/memory',
      '/database',
      '/workspace',
      '/workspace/files',
      '/workspace/prompts',
      '/logs',
      '/doctor',
      '/config',
    ],
  );
});

test('header title lookup includes formerly missing routes and unknown pages', () => {
  assert.equal(titleKeyForPath('/database'), 'nav.database');
  assert.equal(titleKeyForPath('/workspace/files'), 'nav.files');
  assert.equal(titleKeyForPath('/runs'), 'nav.runs');
  assert.equal(titleKeyForPath('/not-a-route'), 'not_found.title');
});

test('unknown routes render a real not-found page instead of redirecting home', () => {
  const app = readSource('../src/App.tsx');
  assert.match(app, /path="\*" element=\{<NotFound \/>}/);
  assert.doesNotMatch(app, /path="\*" element=\{<Navigate to="\/"/);
});

test('application shell uses a mobile drawer and removes mobile content margins', () => {
  const layout = readSource('../src/components/layout/Layout.tsx');
  const sidebar = readSource('../src/components/layout/Sidebar.tsx');
  const header = readSource('../src/components/layout/Header.tsx');

  assert.match(layout, /md:ml-14/);
  assert.match(layout, /md:ml-60/);
  assert.match(layout, /id="main-content"/);
  assert.match(layout, /className="skip-link"/);
  assert.match(sidebar, /md:translate-x-0/);
  assert.match(sidebar, /-translate-x-full/);
  assert.match(header, /aria-controls="primary-navigation"/);
});

test('global styles preserve focus and reduced-motion preferences', () => {
  const styles = readSource('../src/index.css');

  assert.match(styles, /\*:focus-visible/);
  assert.match(styles, /\.skip-link:focus-visible/);
  assert.match(styles, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(styles, /animation-duration: 0\.01ms !important/);
});

test('locale updates rerender shell translations and synchronize document language', () => {
  const i18n = readSource('../src/lib/i18n.ts');
  const header = readSource('../src/components/layout/Header.tsx');
  const sidebar = readSource('../src/components/layout/Sidebar.tsx');

  assert.match(i18n, /useSyncExternalStore<Locale>/);
  assert.match(i18n, /document\.documentElement\.lang = locale/);
  assert.match(i18n, /localeListeners\.forEach/);
  assert.match(header, /const \{ t \} = useLocale\(\)/);
  assert.match(sidebar, /const \{ t \} = useLocale\(\)/);
});
