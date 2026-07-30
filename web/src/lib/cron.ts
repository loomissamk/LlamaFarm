export type IntervalUnit = 'seconds' | 'minutes' | 'hours';

const UNIT_MS: Record<IntervalUnit, number> = {
  seconds: 1_000,
  minutes: 60_000,
  hours: 3_600_000,
};

export function intervalToMs(value: number, unit: IntervalUnit): number {
  if (!Number.isFinite(value) || value <= 0) return 0;
  return Math.round(value * UNIT_MS[unit]);
}

export function intervalFromMs(milliseconds: number): {
  value: number;
  unit: IntervalUnit;
} {
  if (milliseconds > 0 && milliseconds % UNIT_MS.hours === 0) {
    return { value: milliseconds / UNIT_MS.hours, unit: 'hours' };
  }
  if (milliseconds > 0 && milliseconds % UNIT_MS.minutes === 0) {
    return { value: milliseconds / UNIT_MS.minutes, unit: 'minutes' };
  }
  return { value: milliseconds / UNIT_MS.seconds, unit: 'seconds' };
}

export function formatInterval(milliseconds: number): string {
  const { value, unit } = intervalFromMs(milliseconds);
  const singular = unit.slice(0, -1);
  return `${value} ${value === 1 ? singular : unit}`;
}
