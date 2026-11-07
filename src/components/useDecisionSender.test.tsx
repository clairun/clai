import { describe, expect, it, vi } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import { useDecisionSender } from './useDecisionSender';

/** A submit that waits until the test settles it. */
const pendingSubmit = () => {
  const calls: { decision: string; resolve: () => void; reject: (e: unknown) => void }[] = [];
  const submit = vi.fn(
    (decision: string) => new Promise<void>((resolve, reject) => calls.push({ decision, resolve, reject })),
  );
  return { submit, calls };
};

describe('useDecisionSender', () => {
  it('does not send the decision twice', async () => {
    const { submit, calls } = pendingSubmit();
    const onDecided = vi.fn();
    const { result } = renderHook(() => useDecisionSender(submit, onDecided));
    // Both land before React re-renders, so a card's buttons are not disabled yet.
    act(() => {
      void result.current.send('allowOnce');
      void result.current.send('alwaysAllow');
    });
    expect(submit.mock.calls).toEqual([['allowOnce']]);
    expect(result.current.isSubmitting).toBe(true);
    await act(async () => calls[0]!.resolve());
    expect(onDecided).toHaveBeenCalledTimes(1);
    expect(result.current.isSubmitting).toBe(false);
  });

  it('reports a refused decision and lets the card answer again', async () => {
    const { submit, calls } = pendingSubmit();
    const onDecided = vi.fn();
    const { result } = renderHook(() => useDecisionSender(submit, onDecided));
    act(() => void result.current.send('alwaysAllow'));
    await act(async () => calls[0]!.reject('Could not save: disk full'));
    expect(result.current.error).toBe('Could not save: disk full');
    expect(onDecided).not.toHaveBeenCalled();

    act(() => void result.current.send('allowOnce'));
    expect(result.current.error).toBeNull();
    await act(async () => calls[1]!.resolve());
    expect(submit.mock.calls).toEqual([['alwaysAllow'], ['allowOnce']]);
    expect(onDecided).toHaveBeenCalledTimes(1);
  });

  it('takes a request that already left the queue as resolved, and sends nothing more', async () => {
    const { submit, calls } = pendingSubmit();
    const onDecided = vi.fn();
    const { result } = renderHook(() => useDecisionSender(submit, onDecided));
    act(() => void result.current.send('allowRun'));
    await act(async () =>
      calls[0]!.reject('No pending network access request `net-1` (already resolved or cleared)'),
    );
    expect(result.current.resolved).toBe(true);
    expect(result.current.error).toBeNull();
    expect(onDecided).not.toHaveBeenCalled();
    act(() => void result.current.send('deny'));
    expect(submit).toHaveBeenCalledTimes(1);
  });

  it('reads the message of an Error, and names a failure that has none', async () => {
    const { submit, calls } = pendingSubmit();
    const { result } = renderHook(() => useDecisionSender(submit));
    act(() => void result.current.send('deny'));
    await act(async () => calls[0]!.reject(new Error('channel closed')));
    expect(result.current.error).toBe('channel closed');
    act(() => void result.current.send('deny'));
    await act(async () => calls[1]!.reject(undefined));
    expect(result.current.error).toBe('Failed to submit decision');
  });
});
