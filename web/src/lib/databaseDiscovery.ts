import type { DbDiscoveryResult } from '../types/api';

export interface LatestRequest {
  isCurrent: () => boolean;
  cancel: () => void;
}

/**
 * Tracks the latest async request in a UI lifecycle. Starting or explicitly
 * invalidating a request makes every older completion stale.
 */
export class LatestRequestLifecycle {
  private generation = 0;

  begin(): LatestRequest {
    const generation = ++this.generation;
    return {
      isCurrent: () => generation === this.generation,
      cancel: () => {
        if (generation === this.generation) this.generation += 1;
      },
    };
  }

  invalidate(): void {
    this.generation += 1;
  }
}

/**
 * Select a newly connected database first, then any connected database. When
 * every usable probe needs credentials/routing, select that saved connection
 * so the dashboard can immediately show Update connection and Retry.
 */
export function pickDiscoveredConnection(results: DbDiscoveryResult[]): string | null {
  const priorities = [
    (result: DbDiscoveryResult) => result.status === 'connected' && result.newly_added,
    (result: DbDiscoveryResult) => result.status === 'connected',
    (result: DbDiscoveryResult) =>
      result.status === 'needs_configuration' && result.newly_added,
    (result: DbDiscoveryResult) => result.status === 'needs_configuration',
  ];

  for (const matches of priorities) {
    const result = results.find(
      (candidate) => Boolean(candidate.connection_name) && matches(candidate),
    );
    if (result?.connection_name) return result.connection_name;
  }
  return null;
}
