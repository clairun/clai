import { describe, expect, it } from 'vitest';
import {
  describeCron,
  formatIntervalMinutes,
  formatIntervalShort,
  formatScheduleCadence,
  formatScheduleDetail,
  formatTimeUntil,
} from './scheduleFormat';

describe('formatIntervalMinutes', () => {
  it.each([
    [45, '45 min'],
    [60, '1 h'],
    [90, '1 h 30 min'],
    [120, '2 h'],
    [1440, '24 h'],
    [1, '1 min'],
  ])('%i minutes → %s', (minutes, text) => {
    expect(formatIntervalMinutes(minutes)).toBe(text);
  });

  it('has no label for a non-positive or non-finite interval', () => {
    expect(formatIntervalMinutes(0)).toBeNull();
    expect(formatIntervalMinutes(-5)).toBeNull();
    expect(formatIntervalMinutes(Number.NaN)).toBeNull();
  });

  it('never abbreviates minutes to a bare "m" that uppercasing turns into months', () => {
    for (const minutes of [5, 45, 90, 120, 1500]) {
      expect(formatIntervalMinutes(minutes)).not.toMatch(/\dm\b|\bm\b/i);
    }
  });
});

describe('formatIntervalShort', () => {
  it('uses one unit', () => {
    expect(formatIntervalShort(120)).toBe('2 h');
    expect(formatIntervalShort(90)).toBe('90 min');
    expect(formatIntervalShort(1440)).toBe('24 h');
    expect(formatIntervalShort(1500)).toBe('25 h');
    expect(formatIntervalShort(1530)).toBe('1530 min');
    expect(formatIntervalShort(0)).toBeNull();
  });
});

describe('describeCron', () => {
  it.each([
    ['0 18 * * *', 'daily 18:00'],
    ['30 7 * * *', 'daily 07:30'],
    ['0 * * * *', 'hourly'],
    ['15 * * * *', 'hourly at :15'],
    ['*/15 * * * *', 'every 15 min'],
    ['* * * * *', 'every minute'],
    ['0 */6 * * *', 'every 6 h'],
    ['0 9 * * MON-FRI', 'weekdays 09:00'],
    ['0 10 * * sat,sun', 'weekends 10:00'],
    ['0 9 * * MON', 'Mondays 09:00'],
    ['0 9 * * MONDAY', 'Mondays 09:00'],
    ['0 9 * * monday-friday', 'weekdays 09:00'],
    ['00 18 * * *', 'daily 18:00'],
    ['*/30 * * * *', 'every 30 min'],
    ['0 */12 * * *', 'every 12 h'],
    ['*/1 * * * *', 'every minute'],
    ['0 */1 * * *', 'hourly'],
    ['  0   18 * * *  ', 'daily 18:00'],
  ])('%s → %s', (expression, text) => {
    expect(describeCron(expression)).toBe(text);
  });

  it.each([
    // Numeric days: the scheduler numbers them from Sunday = 1, so naming
    // them by Vixie's convention would be wrong; left raw.
    '0 9 * * 1-5',
    '0 9 * * 1',
    '0 0 1 * *',
    '0 0 * 1 *',
    '0,30 9 * * *',
    '0 9-17 * * *',
    '5 */2 * * *',
    // Steps that do not divide the hour or day leave an uneven gap at the
    // wrap (20:00 → 00:00 for */5 hours), so they are not "every N".
    '*/7 * * * *',
    '*/45 * * * *',
    '0 */5 * * *',
    '0 */7 * * *',
    '*/0 * * * *',
    '0 */0 * * *',
    '*/60 * * * *',
    '0 */24 * * *',
    '0 25 * * *',
    '0 18 * *',
    '0 0 18 * * *',
    '',
  ])('does not describe %j', (expression) => {
    expect(describeCron(expression)).toBeNull();
  });
});

describe('formatScheduleCadence / formatScheduleDetail', () => {
  it('describes an interval', () => {
    const kind = { type: 'interval', intervalMinutes: 120 } as const;
    expect(formatScheduleCadence(kind)).toBe('every 2 h');
    expect(formatScheduleDetail(kind)).toBe('every 2 h');
  });

  it('describes a common cron and keeps the raw expression and timezone in the detail', () => {
    const kind = { type: 'cron', expression: '0 18 * * *', timezone: 'Europe/Madrid' } as const;
    expect(formatScheduleCadence(kind, 'Europe/Madrid')).toBe('daily 18:00');
    expect(formatScheduleDetail(kind)).toBe('daily 18:00 (cron: 0 18 * * *, Europe/Madrid)');
  });

  it('names the zone in the cadence only when it is not this machine\'s', () => {
    const kind = { type: 'cron', expression: '0 18 * * *', timezone: 'UTC' } as const;
    expect(formatScheduleCadence(kind, 'UTC')).toBe('daily 18:00');
    expect(formatScheduleCadence(kind, 'Europe/Madrid')).toBe('daily 18:00 · UTC');
    expect(formatScheduleCadence({ type: 'interval', intervalMinutes: 60 }, 'Europe/Madrid')).toBe('every 1 h');
    // The detail already names the zone once.
    expect(formatScheduleDetail(kind)).toBe('daily 18:00 (cron: 0 18 * * *, UTC)');
  });

  it('treats alias spellings of one zone as the same zone', () => {
    const cron = (timezone: string) => ({ type: 'cron', expression: '0 18 * * *', timezone }) as const;
    expect(formatScheduleCadence(cron('Etc/UTC'), 'UTC')).toBe('daily 18:00');
    expect(formatScheduleCadence(cron('Asia/Kolkata'), 'Asia/Calcutta')).toBe('daily 18:00');
    expect(formatScheduleCadence(cron('Europe/Kyiv'), 'Europe/Kiev')).toBe('daily 18:00');
    expect(formatScheduleCadence(cron('Not/AZone'), 'UTC')).toBe('daily 18:00 · Not/AZone');
  });

  it('defaults to the runtime zone', () => {
    const local = Intl.DateTimeFormat().resolvedOptions().timeZone;
    const kind = { type: 'cron', expression: '0 18 * * *', timezone: local } as const;
    expect(formatScheduleCadence(kind)).toBe('daily 18:00');
  });

  it('falls back to the raw cron for an unknown shape', () => {
    const kind = { type: 'cron', expression: '0 9 * * 1-5', timezone: 'UTC' } as const;
    expect(formatScheduleCadence(kind, 'UTC')).toBe('cron: 0 9 * * 1-5');
    expect(formatScheduleCadence(kind, 'Europe/Madrid')).toBe('cron: 0 9 * * 1-5 · UTC');
    expect(formatScheduleDetail(kind)).toBe('cron: 0 9 * * 1-5, UTC');
  });

  it('has no cadence without a usable schedule', () => {
    expect(formatScheduleCadence(null)).toBeNull();
    expect(formatScheduleCadence({ type: 'interval', intervalMinutes: 0 })).toBeNull();
    expect(formatScheduleCadence({ type: 'cron', expression: '  ', timezone: 'UTC' })).toBeNull();
  });
});

describe('formatTimeUntil', () => {
  it.each([
    [0, 'due now'],
    [-5, 'due now'],
    [45, 'in 45 s'],
    [45 * 60, 'in 45 min'],
    [2 * 3600 + 59, 'in 2 h'],
    [3 * 86400, 'in 3 d'],
    [BigInt(120), 'in 2 min'],
  ])('%s s → %s', (seconds, text) => {
    expect(formatTimeUntil(seconds)).toBe(text);
  });

  it('has no text without a next run', () => {
    expect(formatTimeUntil(null)).toBeNull();
    expect(formatTimeUntil(undefined)).toBeNull();
    expect(formatTimeUntil(Number.NaN)).toBeNull();
  });
});
