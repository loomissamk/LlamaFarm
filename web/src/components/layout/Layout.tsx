import { useCallback, useEffect, useRef, useState } from 'react';
import { Outlet, useMatch } from 'react-router-dom';
import Sidebar from '@/components/layout/Sidebar';
import Header from '@/components/layout/Header';
import AgentChat from '@/pages/AgentChat';
import { useLocale } from '@/lib/i18n';

const SIDEBAR_COLLAPSED_KEY = 'llamafarm.sidebar_collapsed.v1';

function loadSidebarCollapsed(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === '1';
  } catch {
    return false;
  }
}

export default function Layout() {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => loadSidebarCollapsed());
  const [mobileSidebarOpen, setMobileSidebarOpen] = useState(false);
  const isAgentRoute = useMatch({ path: '/agent', end: true }) !== null;
  const [agentMounted, setAgentMounted] = useState(isAgentRoute);
  const mobileMenuButtonRef = useRef<HTMLButtonElement>(null);
  const { t } = useLocale();

  useEffect(() => {
    if (isAgentRoute) {
      setAgentMounted(true);
    }
  }, [isAgentRoute]);

  useEffect(() => {
    if (!mobileSidebarOpen) return;

    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';

    return () => {
      document.body.style.overflow = previousOverflow;
    };
  }, [mobileSidebarOpen]);

  useEffect(() => {
    const desktopQuery = window.matchMedia('(min-width: 768px)');
    const closeMobileSidebar = (event: MediaQueryListEvent) => {
      if (event.matches) setMobileSidebarOpen(false);
    };

    desktopQuery.addEventListener('change', closeMobileSidebar);
    return () => desktopQuery.removeEventListener('change', closeMobileSidebar);
  }, []);

  const toggleSidebar = () => {
    setSidebarCollapsed((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(SIDEBAR_COLLAPSED_KEY, next ? '1' : '0');
      } catch {
        // localStorage unavailable — fail silently
      }
      return next;
    });
  };

  const closeMobileSidebar = useCallback((restoreFocus = true) => {
    setMobileSidebarOpen(false);
    if (restoreFocus) {
      requestAnimationFrame(() => mobileMenuButtonRef.current?.focus());
    }
  }, []);

  return (
    <div className="min-h-screen bg-gray-950 text-white">
      <a href="#main-content" className="skip-link">
        {t('a11y.skip_to_content')}
      </a>

      <Sidebar
        collapsed={sidebarCollapsed}
        mobileOpen={mobileSidebarOpen}
        onMobileClose={closeMobileSidebar}
        onNavigate={() => closeMobileSidebar(false)}
        onToggle={toggleSidebar}
      />

      <div
        className={`flex min-h-screen min-w-0 flex-col transition-[margin] duration-200 ${
          sidebarCollapsed ? 'md:ml-14' : 'md:ml-60'
        }`}
      >
        <Header
          mobileMenuButtonRef={mobileMenuButtonRef}
          mobileMenuOpen={mobileSidebarOpen}
          onMobileMenuOpen={() => setMobileSidebarOpen(true)}
        />

        <main id="main-content" tabIndex={-1} className="flex-1 overflow-y-auto">
          {(agentMounted || isAgentRoute) && (
            <div className={isAgentRoute ? '' : 'hidden'}>
              <AgentChat />
            </div>
          )}
          {!isAgentRoute && <Outlet />}
        </main>
      </div>
    </div>
  );
}
