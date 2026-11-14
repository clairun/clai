/**
 * The one place that turns a workspace schedule into words, so the header,
 * the rail and settings agree. Units are spelled out ("2 h", "45 min")
 * because a bare "m" reads as months once a stylesheet uppercases it.
 */
import type { ScheduleKind } from '../generated/bindings';

const validMinutes = (minutes: number): boolean => Number.isFinite(minutes) && minutes > 0;

/** "45 min", "2 h", "1 h 30 min". Null for a non-positive or non-finite value. */
export const formatIntervalMinutes = (minutes: number): string | null => {
  if (!validMinutes(minutes)) return null;
  const whole = Math.round(minutes);
  const hours = Math.floor(whole / 60);
  const rest = whole % 60;
  if (hours === 0) return `${rest} min`;
  return rest === 0 ? `${hours} h` : `${hours} h ${rest} min`;
};

/** At most one unit, for tight spots like a rail row: "2 h" or "90 min". */
export const formatIntervalShort = (minutes: number): string | null => {
  if (!validMinutes(minutes)) return null;
  const whole = Math.round(minutes);
  return whole % 60 === 0 ? `${whole / 60} h` : `${whole} min`;
};

const pad2 = (value: number): string => String(value).padStart(2, '0');

const integerIn = (field: string, min: number, max: number): number | null => {
  if (!/^\d{1,2}$/.test(field)) return null;
  const value = Number(field);
  return value >= min && value <= max ? value : null;
};

// A step that does not divide the cycle evenly restarts at the wrap:
// `0 */5 * * *` fires at 20:00 and then 00:00, four hours later, so it is not
// "every 5 h". Those are left to the raw expression.
const evenStepOf = (field: string, cycle: number): number | null => {
  const match = /^\*\/(\d{1,2})$/.exec(field);
  if (!match) return null;
  const step = Number(match[1]);
  return step >= 1 && step < cycle && cycle % step === 0 ? step : null;
};

const DAY_NAMES: Record<string, string> = {
  SUN: 'Sundays',
  MON: 'Mondays',
  TUE: 'Tuesdays',
  WED: 'Wednesdays',
  THU: 'Thursdays',
  FRI: 'Fridays',
  SAT: 'Saturdays',
};

const FULL_DAY_NAMES: Record<string, string> = {
  SUNDAY: 'SUN',
  MONDAY: 'MON',
  TUESDAY: 'TUE',
  WEDNESDAY: 'WED',
  THURSDAY: 'THU',
  FRIDAY: 'FRI',
  SATURDAY: 'SAT',
};

// Day numbers follow standard cron, as the scheduler reads them: 0 and 7 are
// Sunday, 1-6 Monday-Saturday. They become names, so `1-5` reads as `MON-FRI`.
// Only one day, `MON-FRI`, or the list `SAT,SUN` (in either order) is
// described; anything else (a step, a range other than `MON-FRI`, 8) stays raw.
const DAY_NUMBERS = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT', 'SUN'];

const describeDays = (field: string): string | null => {
  const upper = field
    .toUpperCase()
    .replace(/[A-Z]+/g, (name) => FULL_DAY_NAMES[name] ?? name)
    .replace(/\d+/g, (day) => DAY_NUMBERS[Number(day)] ?? day);
  if (upper === 'MON-FRI') return 'weekdays';
  if (upper === 'SAT,SUN' || upper === 'SUN,SAT') return 'weekends';
  return DAY_NAMES[upper] ?? null;
};

/**
 * Plain words for the common 5-field cron shapes — every minute, every N
 * minutes or hours, hourly, daily, weekdays, weekends, one named day.
 * Null for anything else; callers show the raw expression then.
 */
export const describeCron = (expression: string): string | null => {
  const fields = expression.trim().split(/\s+/);
  if (fields.length !== 5) return null;
  const [minute = '', hour = '', dayOfMonth = '', month = '', dayOfWeek = ''] = fields;
  if (dayOfMonth !== '*' || month !== '*') return null;

  if (dayOfWeek === '*') {
    if (minute === '*' && hour === '*') return 'every minute';
    const minuteStep = evenStepOf(minute, 60);
    if (minuteStep !== null && hour === '*') {
      return minuteStep === 1 ? 'every minute' : `every ${minuteStep} min`;
    }
  }

  const atMinute = integerIn(minute, 0, 59);
  if (atMinute === null) return null;

  if (dayOfWeek === '*') {
    if (hour === '*') return atMinute === 0 ? 'hourly' : `hourly at :${pad2(atMinute)}`;
    const hourStep = evenStepOf(hour, 24);
    if (hourStep !== null && atMinute === 0) {
      return hourStep === 1 ? 'hourly' : `every ${hourStep} h`;
    }
  }

  const atHour = integerIn(hour, 0, 23);
  if (atHour === null) return null;
  const time = `${pad2(atHour)}:${pad2(atMinute)}`;
  if (dayOfWeek === '*') return `daily ${time}`;
  const days = describeDays(dayOfWeek);
  return days ? `${days} ${time}` : null;
};

const cadenceWords = (kind: ScheduleKind | null | undefined): string | null => {
  if (!kind || typeof kind !== 'object') return null;
  if (kind.type === 'interval') {
    const interval = formatIntervalMinutes(Number(kind.intervalMinutes ?? 0));
    return interval ? `every ${interval}` : null;
  }
  if (kind.type === 'cron') {
    const expression = (kind.expression || '').trim();
    if (!expression) return null;
    return describeCron(expression) ?? `cron: ${expression}`;
  }
  return null;
};

const canonicalZones = new Map<string, string>();

// The stored zone comes from the backend's IANA lookup and the local one from
// Intl, which renames some zones (Etc/UTC → UTC, Asia/Kolkata →
// Asia/Calcutta); compare Intl's spelling of both. An unknown name stays as is.
const canonicalZone = (zone: string): string => {
  let canonical = canonicalZones.get(zone);
  if (canonical === undefined) {
    try {
      canonical = new Intl.DateTimeFormat('en', { timeZone: zone }).resolvedOptions().timeZone;
    } catch {
      canonical = zone;
    }
    canonicalZones.set(zone, canonical);
  }
  return canonical;
};

const hostTimeZone = (): string | undefined => {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return undefined;
  }
};

/**
 * The cadence in words: "every 2 h", "daily 18:00", or "cron: <expr>" when
 * the expression is not one `describeCron` knows. A cron schedule in another
 * zone than this machine's names it: "daily 18:00 · UTC". Null when there is
 * no schedule.
 */
export const formatScheduleCadence = (
  kind: ScheduleKind | null | undefined,
  localTimeZone: string | undefined = hostTimeZone(),
): string | null => {
  const words = cadenceWords(kind);
  if (!words || kind?.type !== 'cron') return words;
  const timezone = (kind.timezone || '').trim();
  const foreign = timezone && (!localTimeZone || canonicalZone(timezone) !== canonicalZone(localTimeZone));
  return foreign ? `${words} · ${timezone}` : words;
};

/**
 * The full text for a tooltip: the cadence plus, for cron, the raw expression
 * and its timezone, so the words never hide what is actually configured.
 */
export const formatScheduleDetail = (kind: ScheduleKind | null | undefined): string | null => {
  const cadence = cadenceWords(kind);
  if (!cadence || kind?.type !== 'cron') return cadence;
  const expression = kind.expression.trim();
  const timezone = (kind.timezone || '').trim();
  const raw = `cron: ${expression}`;
  const configured = timezone ? `${raw}, ${timezone}` : raw;
  return cadence === raw ? configured : `${cadence} (${configured})`;
};

/**
 * "in 45 min", "in 2 h", "due now" — the countdown to a run, in the same
 * units as the cadence. Null when there is no next run.
 */
export const formatTimeUntil = (seconds: number | bigint | null | undefined): string | null => {
  if (seconds == null) return null;
  const value = Number(seconds);
  if (!Number.isFinite(value)) return null;
  if (value <= 0) return 'due now';
  if (value < 60) return `in ${Math.floor(value)} s`;
  if (value < 3600) return `in ${Math.floor(value / 60)} min`;
  if (value < 86400) return `in ${Math.floor(value / 3600)} h`;
  return `in ${Math.floor(value / 86400)} d`;
};
