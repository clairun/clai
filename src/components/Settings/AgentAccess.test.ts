import { describe, expect, it } from 'vitest';
import type { SandboxStatus } from '../../generated/bindings';
import { accessSummary, type AccessDraft } from './AgentAccess';

const DRAFT: AccessDraft = {
  grants: [],
  shellMode: 'restricted',
  askFirst: [],
  web: true,
  sessionBusMode: 'keyring_only',
  networkAccess: null,
};

const LINUX: SandboxStatus = {
  available: true, backend: 'bubblewrap', allowlistAvailable: true, allowlistUnavailableReason: null,
  socketIsolationAvailable: true, socketIsolationUnavailableReason: null,
  sessionBusFilterAvailable: true, sessionBusFilterUnavailableReason: null,
};

const summary = (draft: Partial<AccessDraft>, sandbox: SandboxStatus | null = LINUX) =>
  accessSummary({ ...DRAFT, ...draft }, sandbox);
const bus = (draft: Partial<AccessDraft>, sandbox: SandboxStatus | null = LINUX) =>
  summary(draft, sandbox).find((line) => line.startsWith('Bus'));

describe('accessSummary', () => {
  it('names the bus an agent gets', () => {
    expect(bus({ sessionBusMode: 'deny' })).toBe('Bus off');
    expect(bus({ sessionBusMode: 'keyring_only' })).toBe('Bus: keyring only');
    expect(bus({ sessionBusMode: 'full' })).toBe('Bus: full');
  });

  it('says keyring only is no bus where the bus cannot be filtered', () => {
    const unfiltered = { ...LINUX, sessionBusFilterAvailable: false, sessionBusFilterUnavailableReason: 'x' };
    expect(bus({ sessionBusMode: 'keyring_only' }, unfiltered)).toBe('Bus off (keyring only unavailable here)');
    expect(bus({ sessionBusMode: 'full' }, unfiltered)).toBe('Bus: full');
    expect(bus({ sessionBusMode: 'deny' }, unfiltered)).toBe('Bus off');
  });

});
