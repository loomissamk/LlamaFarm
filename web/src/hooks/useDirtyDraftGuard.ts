import { useEffect } from 'react';

export function useDirtyDraftGuard(
  dirty: boolean,
  message = 'You have unsaved changes. Leave this page and discard them?',
) {
  useEffect(() => {
    if (!dirty) return;

    const historyIndex = (state: unknown): number | null => {
      if (typeof state !== 'object' || state === null || !('idx' in state)) return null;
      const index = (state as { idx?: unknown }).idx;
      return typeof index === 'number' ? index : null;
    };
    let currentHistoryIndex = historyIndex(window.history.state);
    let restoringHistory = false;

    const handleBeforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = '';
    };

    const handleDocumentClick = (event: MouseEvent) => {
      if (
        event.defaultPrevented ||
        event.button !== 0 ||
        event.metaKey ||
        event.ctrlKey ||
        event.shiftKey ||
        event.altKey
      ) {
        return;
      }

      const target = event.target;
      const anchor = target instanceof Element ? target.closest<HTMLAnchorElement>('a[href]') : null;
      if (!anchor || anchor.target === '_blank' || anchor.hasAttribute('download')) return;

      const destination = new URL(anchor.href, window.location.href);
      const current = new URL(window.location.href);
      const changesLocation =
        destination.origin !== current.origin ||
        destination.pathname !== current.pathname ||
        destination.search !== current.search ||
        destination.hash !== current.hash;

      if (changesLocation && !window.confirm(message)) {
        event.preventDefault();
        event.stopPropagation();
      }
    };

    const handlePopState = (event: PopStateEvent) => {
      const nextHistoryIndex = historyIndex(event.state);
      if (restoringHistory) {
        restoringHistory = false;
        currentHistoryIndex = nextHistoryIndex;
        return;
      }

      if (window.confirm(message)) {
        currentHistoryIndex = nextHistoryIndex;
        return;
      }

      const restoreDelta =
        currentHistoryIndex !== null && nextHistoryIndex !== null
          ? currentHistoryIndex - nextHistoryIndex
          : 1;
      if (restoreDelta !== 0) {
        restoringHistory = true;
        window.history.go(restoreDelta);
      }
    };

    window.addEventListener('beforeunload', handleBeforeUnload);
    window.addEventListener('popstate', handlePopState);
    document.addEventListener('click', handleDocumentClick, true);
    return () => {
      window.removeEventListener('beforeunload', handleBeforeUnload);
      window.removeEventListener('popstate', handlePopState);
      document.removeEventListener('click', handleDocumentClick, true);
    };
  }, [dirty, message]);
}
