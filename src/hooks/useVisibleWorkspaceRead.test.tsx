import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const native = vi.hoisted(() => ({ visible: true, minimized: false, unread: false, resize: null as null | (() => void), mark: vi.fn(), unreadRead: vi.fn(), visibleRead: vi.fn(), minimizedRead: vi.fn() }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({
  isVisible: () => native.visibleRead().then(() => native.visible),
  isMinimized: () => native.minimizedRead().then(() => native.minimized),
  onResized: (callback: () => void) => { native.resize = callback; return Promise.resolve(() => { native.resize = null; }); },
}) }));
vi.mock('../workspace/client', () => ({
  markWorkspaceOpened: (id: string) => native.mark(id).then(() => { native.unread = false; }),
  isWorkspaceUnread: (id: string) => native.unreadRead(id).then(() => native.unread),
}));
import { useVisibleWorkspaceRead } from './useVisibleWorkspaceRead';

beforeEach(() => {
  vi.useRealTimers();
  native.visible = true;
  native.minimized = false;
  native.unread = false;
  native.resize = null;
  native.mark.mockReset().mockResolvedValue(undefined);
  native.unreadRead.mockReset().mockResolvedValue(undefined);
  native.visibleRead.mockReset().mockResolvedValue(undefined);
  native.minimizedRead.mockReset().mockResolvedValue(undefined);
});

describe('visible workspace unread', () => {
  it('marks an unfocused visible workspace and a selected Fleet card', async () => {
    const first = renderHook(({ id }) => useVisibleWorkspaceRead(id, false), { initialProps: { id: 'workspace' } });
    await waitFor(() => expect(native.mark).toHaveBeenCalledWith('workspace'));
    first.rerender({ id: 'fleet-selected' });
    await waitFor(() => expect(native.mark).toHaveBeenCalledWith('fleet-selected'));
    first.unmount();
  });

  it('keeps unread while minimized and catches restore without browser events', async () => {
    native.minimized = true;
    const view = renderHook(() => useVisibleWorkspaceRead('ws', false));
    await waitFor(() => expect(native.minimizedRead).toHaveBeenCalled());
    expect(native.mark).not.toHaveBeenCalled();
    native.minimized = false;
    act(() => native.resize?.());
    await waitFor(() => expect(native.mark).toHaveBeenCalledWith('ws'));
    view.unmount();
  });

  it('detects native restore through the mounted poll when no event arrives', async () => {
    native.minimized = true;
    vi.useFakeTimers();
    const view = renderHook(() => useVisibleWorkspaceRead('ws', false));
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(native.minimizedRead).toHaveBeenCalled();
    native.minimized = false;
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(native.mark).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(native.mark).toHaveBeenCalledTimes(1);
    view.unmount();
    vi.useRealTimers();
  });

  it('clears a completion after minimize and restore both occur between polls', async () => {
    vi.useFakeTimers();
    const view = renderHook(() => useVisibleWorkspaceRead('ws', false));
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(native.mark).toHaveBeenCalledTimes(1);

    native.minimized = true;
    native.unread = true; // A scheduled run completes while minimized.
    native.minimized = false; // No native or browser event, and no poll saw minimization.
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(native.unreadRead).toHaveBeenCalledWith('ws');
    expect(native.mark).toHaveBeenCalledTimes(2);
    expect(native.unread).toBe(false);

    await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
    expect(native.mark).toHaveBeenCalledTimes(2);
    view.unmount();
    vi.useRealTimers();
  });

  it('does not mark a hidden window or an unmounted view after async native reads', async () => {
    let finish!: () => void;
    native.visibleRead.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    const view = renderHook(() => useVisibleWorkspaceRead('ws', false));
    view.unmount();
    await act(async () => { finish(); });
    expect(native.mark).not.toHaveBeenCalled();
    native.visible = false;
    const hidden = renderHook(() => useVisibleWorkspaceRead('ws', false));
    await waitFor(() => expect(native.visibleRead).toHaveBeenCalledTimes(2));
    expect(native.mark).not.toHaveBeenCalled();
    hidden.unmount();
  });

  it('supports duplicate views and rechecks after a run resolves', async () => {
    const a = renderHook(({ revision }) => useVisibleWorkspaceRead('ws', revision), { initialProps: { revision: 0 } });
    const b = renderHook(() => useVisibleWorkspaceRead('ws', 0));
    await waitFor(() => expect(native.mark).toHaveBeenCalledTimes(2));
    a.rerender({ revision: 1 });
    await waitFor(() => expect(native.mark).toHaveBeenCalledTimes(3));
    a.unmount(); b.unmount();
  });
});
