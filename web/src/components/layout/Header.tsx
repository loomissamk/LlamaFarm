import { useEffect, type RefObject } from 'react';
import { Menu } from 'lucide-react';
import { useLocation } from 'react-router-dom';
import { useLocale } from '@/lib/i18n';
import { titleKeyForPath } from '@/lib/navigation';

interface HeaderProps {
  mobileMenuButtonRef: RefObject<HTMLButtonElement | null>;
  mobileMenuOpen: boolean;
  onMobileMenuOpen: () => void;
}

export default function Header({
  mobileMenuButtonRef,
  mobileMenuOpen,
  onMobileMenuOpen,
}: HeaderProps) {
  const location = useLocation();
  const { t } = useLocale();
  const title = t(titleKeyForPath(location.pathname));

  useEffect(() => {
    document.title = `${title} · LlamaFarm`;
  }, [title]);

  return (
    <header className="flex h-14 items-center justify-between gap-3 border-b border-gray-700 bg-gray-800 px-3 sm:px-4 md:px-6">
      <div className="flex min-w-0 items-center gap-2.5">
        <button
          ref={mobileMenuButtonRef}
          type="button"
          onClick={onMobileMenuOpen}
          className="inline-flex min-h-11 min-w-11 items-center justify-center rounded-lg text-gray-300 transition-colors hover:bg-gray-700 hover:text-white md:hidden"
          aria-controls="primary-navigation"
          aria-expanded={mobileMenuOpen}
          aria-label={t('nav.open_menu')}
        >
          <Menu className="h-5 w-5" aria-hidden="true" />
        </button>
        <h1 className="truncate text-base font-semibold text-white sm:text-lg">{title}</h1>
      </div>
      <span className="hidden text-xs font-medium uppercase tracking-[0.24em] text-gray-500 sm:block">
        {t('app.runtime')}
      </span>
    </header>
  );
}
