import { useEffect, useState } from 'react';
import { getSandboxStatus } from '../api/client';
import type { SandboxStatus } from '../generated/bindings';

// The sandbox cannot change while the app runs, so one request serves every caller.
let request: Promise<SandboxStatus> | null = null;

/** What the sandbox can enforce on this PC; null until the backend answers, or if it cannot. */
export function useSandboxStatus(): SandboxStatus | null {
  const [status, setStatus] = useState<SandboxStatus | null>(null);
  useEffect(() => {
    let live = true;
    request ??= getSandboxStatus().catch((error: unknown) => {
      request = null;
      throw error;
    });
    request.then(
      (value) => { if (live) setStatus(value); },
      () => {},
    );
    return () => { live = false; };
  }, []);
  return status;
}

/** Tests only: forget the cached answer. */
export const resetSandboxStatusForTests = () => { request = null; };
