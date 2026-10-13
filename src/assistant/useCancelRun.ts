import { useCallback, useState } from 'react';

interface UseCancelRunOptions {
  /** Ask the backend to stop. Resolving only means the request was accepted. */
  cancel: () => Promise<unknown>;
  /** Whether the run behind `key` is still going; the latch only shows while it is. */
  isActive: boolean;
  /** Identifies one run (e.g. `workspaceId:runId`). Kept per key, so a latch survives switching away and back. */
  key: string;
}

/** Per key: `null` while a stop is latched, the message after a failed one. */
type StopState = ReadonlyMap<string, string | null>;

/**
 * A Stop button's click latch. Cancelling only signals the run, which stops at
 * its next checkpoint, so the button stays stopping until the caller reports
 * the run inactive. A rejected cancel releases the latch and keeps the error.
 */
export function useCancelRun({ cancel, isActive, key }: UseCancelRunOptions) {
  const [states, setStates] = useState<StopState>(() => new Map());
  const latched = states.has(key) && states.get(key) === null;

  const stop = useCallback(async () => {
    if (!isActive || latched) return;
    setStates((prev) => new Map(prev).set(key, null));
    try {
      await cancel();
    } catch (err) {
      setStates((prev) => new Map(prev).set(key, describeError(err)));
    }
  }, [cancel, isActive, key, latched]);

  return { stop, stopping: isActive && latched, error: states.get(key) ?? null };
}

const describeError = (err: unknown): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to stop.';
