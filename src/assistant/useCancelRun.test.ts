import { describe, expect, it } from 'vitest';
import { act, renderHook } from '@testing-library/react';

import { useCancelRun } from './useCancelRun';

type Props = { cancel: () => Promise<unknown>; isActive: boolean; key: string };

const deferred = () => {
  let resolve!: () => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

describe('useCancelRun', () => {
  it('useCancelRun latches until inactive', async () => {
    const call = deferred();
    let calls = 0;
    const cancel = () => {
      calls += 1;
      return call.promise;
    };
    const { result, rerender } = renderHook((props: Props) => useCancelRun(props), {
      initialProps: { cancel, isActive: true, key: 'ws-a:run-1' },
    });

    await act(async () => {
      void result.current.stop();
    });
    expect(result.current.stopping).toBe(true);

    // Accepting the cancel does not end the run: still stopping.
    await act(async () => call.resolve());
    expect(result.current.stopping).toBe(true);
    await act(async () => {
      await result.current.stop();
    });
    expect(calls).toBe(1);

    // Switching to another run and back keeps the latch for the first one.
    rerender({ cancel, isActive: true, key: 'ws-b:run-2' });
    expect(result.current.stopping).toBe(false);
    rerender({ cancel, isActive: true, key: 'ws-a:run-1' });
    expect(result.current.stopping).toBe(true);

    rerender({ cancel, isActive: false, key: 'ws-a:run-1' });
    expect(result.current.stopping).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it('useCancelRun clears latch and reports error on reject', async () => {
    const call = deferred();
    const { result } = renderHook((props: Props) => useCancelRun(props), {
      initialProps: { cancel: () => call.promise, isActive: true, key: 'ws-a:run-1' },
    });

    await act(async () => {
      void result.current.stop();
    });
    expect(result.current.stopping).toBe(true);

    await act(async () => call.reject(new Error('run already gone')));
    expect(result.current.stopping).toBe(false);
    expect(result.current.error).toBe('run already gone');
  });
});
