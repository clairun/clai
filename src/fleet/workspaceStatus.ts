/**
 * Shared workspace status/label helpers used by the Fleet rail (and any
 * other surface that needs to render a workspace's at-a-glance state).
 *
 * Extracted from the original Fleet card grid so the rail and the grid
 * agree on status derivation and formatting. Pure functions only — no
 * React, no side effects.
 */
import type { ScheduleKind, WorkspaceListEntry } from '../generated/bindings';

export type CardStatus = 'idle' | 'running' | 'attention' | 'critical';

export const CARD_STATUS_LABEL: Record<CardStatus, string> = {
  idle: 'Idle',
  running: 'Running',
  attention: 'Needs attention',
  critical: 'Failed task',
};

// Fleet card counts are i64 on the wire (ts-rs types them bigint) but
// arrive as JS numbers. Coerce before arithmetic to dodge bigint/number
// operator friction.
export const num = (value: number | bigint | null | undefined): number => Number(value ?? 0);

/**
 * Single-valued, priority-ordered card status. Critical (a failed task)
 * outranks attention (pending approval / blocked task), which outranks
 * running, which outranks idle.
 */
export const deriveCardStatus = (
  ws: WorkspaceListEntry,
  isProcessing: boolean,
  hasPendingApprovals: boolean,
): CardStatus => {
  if (num(ws.failedTaskCount) > 0) return 'critical';
  if (hasPendingApprovals || num(ws.blockedTaskCount) > 0) return 'attention';
  if (isProcessing) return 'running';
  return 'idle';
};

/**
 * User-facing cadence label for a workspace's schedule. Returns `null`
 * when no schedule is set. Mirrors the backend's `ScheduleKind` tagged
 * union; falls back to the raw cron expression.
 */
export const formatScheduleLabel = (
  kind: ScheduleKind | null | undefined,
): string | null => {
  if (!kind || typeof kind !== 'object') return null;
  if (kind.type === 'interval') {
    const minutes = Number(kind.intervalMinutes ?? 0);
    if (!Number.isFinite(minutes) || minutes <= 0) return null;
    return `every ${minutes}m`;
  }
  if (kind.type === 'cron') {
    const expr = (kind.expression || '').trim();
    if (!expr) return null;
    return `cron: ${expr}`;
  }
  return null;
};

/** Human-readable countdown to the next scheduled run. */
export const formatNextRun = (seconds: number | bigint | null | undefined): string => {
  if (seconds == null) return '';
  const s = Number(seconds);
  if (s <= 0) return 'Due now';
  if (s < 60) return `Next run in ${s}s`;
  if (s < 3600) return `Next run in ${Math.floor(s / 60)}m`;
  if (s < 86400) return `Next run in ${Math.floor(s / 3600)}h`;
  return `Next run in ${Math.floor(s / 86400)}d`;
};

export const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;
