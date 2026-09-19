import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import { useAppTheme } from './useAppTheme';

const setTheme = (theme: string | null) => {
  if (theme === null) document.documentElement.removeAttribute('data-theme');
  else document.documentElement.setAttribute('data-theme', theme);
};

afterEach(() => setTheme(null));

describe('useAppTheme', () => {
  it('reads the attribute and follows later changes', async () => {
    setTheme('dark');
    const { result } = renderHook(() => useAppTheme());
    expect(result.current).toBe('dark');

    await act(async () => {
      setTheme('light');
      // MutationObserver callbacks run as microtasks.
      await Promise.resolve();
    });
    expect(result.current).toBe('light');
  });

  it('shares one observer across subscribers and drops it with the last one', async () => {
    const observe = vi.spyOn(MutationObserver.prototype, 'observe');
    const disconnect = vi.spyOn(MutationObserver.prototype, 'disconnect');
    const a = renderHook(() => useAppTheme());
    const b = renderHook(() => useAppTheme());
    expect(observe).toHaveBeenCalledTimes(1);

    await act(async () => {
      setTheme('dark');
      await Promise.resolve();
    });
    expect(a.result.current).toBe('dark');
    expect(b.result.current).toBe('dark');

    a.unmount();
    expect(disconnect).not.toHaveBeenCalled();
    b.unmount();
    expect(disconnect).toHaveBeenCalledTimes(1);

    // A later subscriber starts a fresh observer.
    renderHook(() => useAppTheme());
    expect(observe).toHaveBeenCalledTimes(2);
  });
});
