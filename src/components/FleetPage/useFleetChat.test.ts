import { renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

vi.mock('../../assistant/client', () => ({ getSession: vi.fn(async () => null) }));
vi.mock('../../workspace/client', () => ({ getWorkspaceDetails: vi.fn(async () => ({ tasks: [] })) }));

const { useFleetChat } = await import('./useFleetChat');

describe('useFleetChat', () => {
  it('stops loading when the main session no longer exists', async () => {
    const { result } = renderHook(() => useFleetChat('ws', 'deleted-session'));
    expect(result.current.loading).toBe(true);
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toBeNull();
  });
});
