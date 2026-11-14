/**
 * Shared workspace status/label helpers used by the Fleet rail (and any
 * other surface that needs to render a workspace's at-a-glance state).
 *
 * Extracted from the original Fleet card grid so the rail and the grid
 * agree on status derivation and formatting. Pure functions only — no
 * React, no side effects.
 */
import type { WorkspaceListEntry } from '../generated/bindings';

export type CardStatus = 'idle' | 'running' | 'attention';

export const CARD_STATUS_LABEL: Record<CardStatus, string> = {
  idle: 'Idle',
  running: 'Running',
  attention: 'Needs attention',
};

// Fleet card counts are i64 on the wire (ts-rs types them bigint) but
// arrive as JS numbers. Coerce before arithmetic to dodge bigint/number
// operator friction.
export const num = (value: number | bigint | null | undefined): number => Number(value ?? 0);

/**
 * Single-valued, priority-ordered card status. Pending approval outranks
 * running, which outranks idle.
 */
export const deriveCardStatus = (
  ws: WorkspaceListEntry,
  isProcessing: boolean,
  hasPendingApprovals: boolean,
): CardStatus => {
  if (hasPendingApprovals) return 'attention';
  if (isProcessing) return 'running';
  return 'idle';
};

export const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;
