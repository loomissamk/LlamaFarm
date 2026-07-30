import type { DbQueryResult } from '@/types/api';

function csvCell(value: unknown): string {
  if (value === null || value === undefined) return '';
  const text = typeof value === 'object' ? JSON.stringify(value) : String(value);
  return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

export function databaseResultToCsv(result: DbQueryResult): string {
  const rows = [
    result.columns.map(csvCell).join(','),
    ...result.rows.map((row) => row.map(csvCell).join(',')),
  ];
  return `${rows.join('\n')}\n`;
}
