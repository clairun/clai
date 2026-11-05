import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

const listeners = vi.hoisted(() => [] as { name: string; handler: (event: { payload: unknown }) => void }[]);
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, handler: (event: { payload: unknown }) => void) => {
    listeners.push({ name, handler });
    return Promise.resolve(() => {});
  }),
}));

import { usePermissionAttention } from './usePermissionAttention';

const SEEDS: Record<string, Record<string, number>> = {
  list_pending_permission_counts: { 'ws-1': 1 },
  list_pending_path_grant_counts: { 'ws-1': 2, 'ws-2': 1 },
  list_pending_network_access_counts: { 'ws-1': 4, 'ws-3': 1 },
};

const fire = (name: string, payload: unknown) =>
  act(() => {
    for (const listener of listeners.filter((l) => l.name === name)) listener.handler({ payload });
  });

beforeEach(() => {
  listeners.length = 0;
  mockInvoke.mockReset();
  mockInvoke.mockImplementation((cmd: string) => Promise.resolve(SEEDS[cmd] ?? {}));
});

describe('usePermissionAttention', () => {
  it('adds network requests to the attention count of their workspace, from the seed and from events', async () => {
    const { result } = renderHook(() => usePermissionAttention());
    await waitFor(() => expect(result.current).toEqual({ 'ws-1': 7, 'ws-2': 1, 'ws-3': 1 }));

    fire('network-access://attention', { workspaceId: 'ws-2', pendingCount: 2 });
    expect(result.current).toEqual({ 'ws-1': 7, 'ws-2': 3, 'ws-3': 1 });
    fire('network-access://attention', { workspaceId: 'ws-3', pendingCount: 0 });
    expect(result.current).toEqual({ 'ws-1': 7, 'ws-2': 3 });
  });

  it('keeps the count of each kind apart, so one kind reaching zero leaves the others', async () => {
    const { result } = renderHook(() => usePermissionAttention());
    await waitFor(() => expect(result.current['ws-1']).toBe(7));
    fire('permissions://attention', { workspaceId: 'ws-1', pendingCount: 0 });
    fire('path-grants://attention', { workspaceId: 'ws-1', pendingCount: 0 });
    expect(result.current['ws-1']).toBe(4);
  });
});
