import type { DiagResult } from '../types/api';

export type DiagnosticSeverityFilter = 'all' | DiagResult['severity'];

export function diagnosticCategories(results: DiagResult[]): string[] {
  return [...new Set(results.map((result) => result.category))].sort((a, b) =>
    a.localeCompare(b),
  );
}

export function filterDiagnostics(
  results: DiagResult[],
  severity: DiagnosticSeverityFilter,
  category: string,
): DiagResult[] {
  return results.filter(
    (result) =>
      (severity === 'all' || result.severity === severity) &&
      (category === 'all' || result.category === category),
  );
}

export function diagnosticCounts(results: DiagResult[]) {
  return {
    ok: results.filter((result) => result.severity === 'ok').length,
    warn: results.filter((result) => result.severity === 'warn').length,
    error: results.filter((result) => result.severity === 'error').length,
  };
}
