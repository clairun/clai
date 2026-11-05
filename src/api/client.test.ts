import { describe, expect, it, vi } from 'vitest';
import type { WorkspaceAgentUpdateRequest } from '../generated/bindings';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

import { workspaceUpdateAgent } from './client';
import { StaleAgentError } from './errors';

const request = { workspaceId: 'ws', agentId: 'main', expectedUpdatedAt: 1 } as WorkspaceAgentUpdateRequest;

describe('workspaceUpdateAgent', () => {
  it('turns a stale refusal into StaleAgentError and other failures into messages', async () => {
    mockInvoke.mockRejectedValueOnce({ kind: 'stale', message: 'This agent changed after you opened it.' });
    const stale = await workspaceUpdateAgent(request).catch((error: unknown) => error);
    expect(stale).toBeInstanceOf(StaleAgentError);
    expect((stale as Error).message).toBe('This agent changed after you opened it.');

    mockInvoke.mockRejectedValueOnce({ kind: 'failed', message: 'Allowed hosts: `*.*` is not valid.' });
    await expect(workspaceUpdateAgent(request)).rejects.toThrow(
      'Failed to update workspace agent: Allowed hosts: `*.*` is not valid.',
    );
  });
});
