import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));

import AboutSettings from './AboutSettings';

const REASON = 'Development builds do not use installer updates.';

/** Backend responses for the two invokes AboutSettings makes on mount. */
const respond = (status: unknown) => {
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === 'app_version_detail') return Promise.resolve('26.7.12-dev');
    if (cmd === 'get_app_update_status') return Promise.resolve(status);
    return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
  });
};

const unavailableStatus = {
  settings: { autoDownload: true },
  support: {
    supported: false,
    canCheck: false,
    platform: 'linux',
    bundleType: null,
    channel: 'dev',
    reason: REASON,
  },
  // The backend mirrors the support reason into lastCheck.error for
  // fully-unavailable builds; the UI must not render it twice.
  lastCheck: { checkedAt: '2026-07-24T00:00:00Z', update: null, error: REASON },
};

const notifyOnlyStatus = {
  settings: { autoDownload: true },
  support: {
    supported: false,
    canCheck: true,
    platform: 'linux',
    bundleType: 'deb',
    channel: 'flatpak',
    reason: 'This Flatpak build cannot update itself; download new releases from GitHub.',
  },
  lastCheck: { checkedAt: '2026-07-24T00:00:00Z', update: null, error: null },
};

describe('AboutSettings updates panel', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it('shows the unavailable reason exactly once and hides the check button', async () => {
    respond(unavailableStatus);
    render(<AboutSettings />);
    await waitFor(() => expect(screen.getByText('Unavailable')).toBeTruthy());

    expect(screen.getAllByText(REASON)).toHaveLength(1);
    expect(screen.queryByRole('button', { name: /check for updates/i })).toBeNull();
  });

  it('keeps the check button and status line for notify-only builds', async () => {
    respond(notifyOnlyStatus);
    render(<AboutSettings />);
    await waitFor(() => expect(screen.getByText('Notify only')).toBeTruthy());

    expect(screen.getByRole('button', { name: /check for updates/i })).toBeTruthy();
    expect(screen.getByText('CLAI is up to date.')).toBeTruthy();
  });
});
