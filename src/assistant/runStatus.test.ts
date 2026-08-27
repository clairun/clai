import { describe, expect, it } from 'vitest';
import type { AssistantRun, RunStatus } from '../generated/bindings';
import { ACTIVE_RUN_STATUSES, hasActiveAssistantRun } from './runStatus';

const run = (status: RunStatus): AssistantRun =>
  ({ id: `run-${status}`, status }) as unknown as AssistantRun;

describe('runStatus', () => {
  it('keeps the active status vocabulary explicit', () => {
    expect(ACTIVE_RUN_STATUSES).toEqual(['queued', 'running', 'waiting_for_tool']);
  });

  it('matches only non-terminal generated run statuses', () => {
    const activeStatuses: RunStatus[] = ['queued', 'running', 'waiting_for_tool'];
    const terminalStatuses: RunStatus[] = [
      'completed',
      'completed_with_warnings',
      'failed',
      'cancelled',
    ];

    for (const status of activeStatuses) {
      expect(hasActiveAssistantRun([run(status)])).toBe(true);
    }
    for (const status of terminalStatuses) {
      expect(hasActiveAssistantRun([run(status)])).toBe(false);
    }
  });

  it('treats empty and unknown snapshots as inactive', () => {
    expect(hasActiveAssistantRun(undefined)).toBe(false);
    expect(hasActiveAssistantRun(null)).toBe(false);
    expect(
      hasActiveAssistantRun([
        { id: 'run-paused', status: 'paused' } as unknown as AssistantRun,
      ])
    ).toBe(false);
  });
});
