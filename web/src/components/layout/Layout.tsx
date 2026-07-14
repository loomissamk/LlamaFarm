import { useState } from 'react';
import { Outlet } from 'react-router-dom';
import Sidebar from '@/components/layout/Sidebar';
import Header from '@/components/layout/Header';

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
      <Sidebar collapsed={sidebarCollapsed} onToggle={toggleSidebar} />

      <div className={`flex flex-col min-h-screen transition-[margin] duration-200 ${sidebarCollapsed ? 'ml-14' : 'ml-60'}`}>
        <Header />

        <main className="flex-1 overflow-y-auto">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
