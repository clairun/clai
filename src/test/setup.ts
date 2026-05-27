import '@testing-library/jest-dom/vitest';
import { afterEach, vi } from 'vitest';
import { cleanup } from '@testing-library/react';

// jsdom doesn't implement layout APIs that components call on mount.
// Stub the ones our components use so effects don't throw under test.
if (typeof Element !== 'undefined' && !Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = vi.fn();
}

// Reset DOM + every module-level mock between tests so cross-file state
// (zustand store, listeners, timers) can't leak.
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
