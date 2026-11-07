import { useCallback, useEffect, useState } from 'react';
import { getSandboxStatus } from '../api/client';
import type { SandboxStatus } from '../generated/bindings';

// The sandbox cannot change while the app runs, so one request serves every caller.
let request: Promise<SandboxStatus> | null = null;

export interface SandboxStatusState {
  /** Null until the backend answers, or if it could not. */
  status: SandboxStatus | null;
  failed: boolean;
  retry: () => void;
}

/** What the sandbox can enforce on this PC. */
export function useSandboxStatus(): SandboxStatusState {
  const [status, setStatus] = useState<SandboxStatus | null>(null);
  const [failed, setFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    request ??= getSandboxStatus().catch((error: unknown) => {
      request = null;
      throw error;
    });
    request.then(
      (value) => { if (live) setStatus(value); },
      () => { if (live) setFailed(true); },
    );
    return () => { live = false; };
  }, [attempt]);
  const retry = useCallback(() => {
    setFailed(false);
    setAttempt((count) => count + 1);
  }, []);
  return { status, failed, retry };
}

/** Tests only: forget the cached answer. */
export const resetSandboxStatusForTests = () => { request = null; };
