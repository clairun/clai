import { describe, expect, it } from 'vitest';
import type { AssistantRun } from '../generated/bindings';
import { shouldHydrateWorkspaceSession } from './sessionHydration';

const run = (status: AssistantRun['status']): AssistantRun =>
  ({ id: `run-${status}`, status }) as unknown as AssistantRun;

describe('shouldHydrateWorkspaceSession', () => {
  it('hydrates the first time a workspace session appears', () => {
    expect(
      shouldHydrateWorkspaceSession({
        existingSessionPresent: false,
        hasUnloadedUpdate: false,
        existingIsStreaming: false,
        snapshotRuns: [],
      })
    ).toBe(true);
  });

  it('skips message hydration during an active streaming run', () => {
    expect(
      shouldHydrateWorkspaceSession({
        existingSessionPresent: true,
        hasUnloadedUpdate: true,
        existingIsStreaming: true,
        snapshotRuns: [run('running')],
      })
    ).toBe(false);
  });

  it('hydrates a stale streaming session once the snapshot has only terminal runs', () => {
    expect(
      shouldHydrateWorkspaceSession({
        existingSessionPresent: true,
        hasUnloadedUpdate: true,
        existingIsStreaming: true,
        snapshotRuns: [run('completed')],
      })
    ).toBe(true);
  });

  it('hydrates updated idle sessions normally', () => {
    expect(
      shouldHydrateWorkspaceSession({
        existingSessionPresent: true,
        hasUnloadedUpdate: true,
        existingIsStreaming: false,
        snapshotRuns: [run('completed')],
      })
    ).toBe(true);
  });

  it('does not hydrate unchanged existing sessions', () => {
    expect(
      shouldHydrateWorkspaceSession({
        existingSessionPresent: true,
        hasUnloadedUpdate: false,
        existingIsStreaming: false,
        snapshotRuns: [],
      })
    ).toBe(false);
  });
});
