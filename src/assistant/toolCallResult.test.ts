import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const getToolCallResult = vi.hoisted(() =>
  vi.fn((_sessionId: string, toolCallId: string) => Promise.resolve(`output of ${toolCallId}`)),
);
vi.mock('./client', () => ({ getToolCallResult, getToolCallInput: vi.fn() }));

import { clearToolCallResultCache, useToolCallResult } from './toolCallResult';

const call = (id: string) => ({
  id,
  sessionId: 'sess-1',
  status: 'completed' as const,
  completedAt: 1n,
});

describe('useToolCallResult', () => {
  beforeEach(() => {
    clearToolCallResultCache();
    getToolCallResult.mockClear();
  });

  it('keeps a value found in the cache after the entry is evicted', async () => {
    const first = renderHook(() => useToolCallResult(call('tc-0'), true));
    await waitFor(() => expect(first.result.current.state).toBe('ready'));
    first.unmount();

    const remounted = renderHook(() => useToolCallResult(call('tc-0'), true));
    expect(remounted.result.current).toMatchObject({ state: 'ready', value: 'output of tc-0' });
    expect(getToolCallResult).toHaveBeenCalledTimes(1);

    const others = Array.from({ length: 30 }, (_, i) =>
      renderHook(() => useToolCallResult(call(`tc-${i + 1}`), true)),
    );
    for (const other of others) {
      await waitFor(() => expect(other.result.current.state).toBe('ready'));
    }

    act(() => remounted.rerender());
    expect(remounted.result.current).toMatchObject({ state: 'ready', value: 'output of tc-0' });
  });
});
