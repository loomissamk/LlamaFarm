import { useEffect, useState } from 'react';
import { Outlet, useMatch } from 'react-router-dom';
import Sidebar from '@/components/layout/Sidebar';
import Header from '@/components/layout/Header';
import AgentChat from '@/pages/AgentChat';

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
  const [mobileNavOpen, setMobileNavOpen] = useState(false);
  const isAgentRoute = useMatch({ path: '/agent', end: true }) !== null;
  const [agentMounted, setAgentMounted] = useState(isAgentRoute);

  useEffect(() => {
    if (isAgentRoute) {
      setAgentMounted(true);
    }
  }, [isAgentRoute]);

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

  return (
    <div className="min-h-screen bg-gray-950 text-white">
      <Sidebar
        collapsed={sidebarCollapsed}
        onToggle={toggleSidebar}
        mobileOpen={mobileNavOpen}
        onMobileClose={() => setMobileNavOpen(false)}
      />

      <div
        className={`flex flex-col min-h-screen transition-[margin] duration-200 ${
          sidebarCollapsed ? 'md:ml-14' : 'md:ml-60'
        }`}
      >
        <Header onMenuClick={() => setMobileNavOpen(true)} />

        <main className="flex-1 overflow-y-auto">
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
