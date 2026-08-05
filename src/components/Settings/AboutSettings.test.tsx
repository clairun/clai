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

/** Linux deb/rpm with a published update that is still notify-only. The body
 *  copy and button label differ from the generic non-installable branch —
 *  this fixture exercises the platform branch. */
const linuxPkgStatus = {
  support: {
    supported: false,
    canCheck: true,
    platform: 'linux',
    arch: 'x86_64',
    bundleType: 'deb',
    channel: 'linux_pkg',
    reason: 'This Linux install should be updated by its package manager.',
  },
  lastCheck: {
    checkedAt: '2026-07-24T12:00:00Z',
    error: null,
    update: {
      currentVersion: '26.7.12',
      version: '26.8.1',
      date: null,
      body: null,
      // The published version is reported by the feed but the build
      // can't self-update — installs flow through the system package
      // manager.
      installable: false,
      downloaded: false,
    },
  },
};

/** Flatpak is a Linux build too — also notify-only because updates flow
 *  through `flatpak update`, which IS CLAI's package manager here. The
 *  same body copy applies; only the `channel` and the support `reason`
 *  differ. */
const linuxFlatpakStatus = {
  support: {
    supported: false,
    canCheck: true,
    platform: 'linux',
    arch: 'x86_64',
    bundleType: 'flatpak',
    channel: 'flatpak',
    reason: 'Flatpak updates are managed outside CLAI.',
  },
  lastCheck: {
    ...linuxPkgStatus.lastCheck,
  },
};

/** A build that CAN install updates itself (macOS .app / Windows NSIS):
 *  the only channel where a background download happens at all. */
const supportedStatus = {
  support: {
    supported: true,
    canCheck: true,
    platform: 'macos',
    arch: 'arm64',
    bundleType: 'app',
    channel: 'native',
    reason: null,
  },
  lastCheck: { checkedAt: '2026-07-24T00:00:00Z', update: null, error: null },
};

describe('AboutSettings updates panel', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it('renders no Updates panel at all when the build can neither update nor check', async () => {
    respond(unavailableStatus);
    render(<AboutSettings />);
    // Wait until the status has loaded (version renders from the same pass).
    await waitFor(() => expect(screen.getByText('v26.7.12-dev')).toBeTruthy());

    expect(screen.queryByText('Updates')).toBeNull();
    expect(screen.queryByText('Unavailable')).toBeNull();
    expect(screen.queryByText(REASON)).toBeNull();
    expect(screen.queryByRole('button', { name: /check for updates/i })).toBeNull();
  });

  it('offers no update preferences even on self-updating builds', async () => {
    // Background downloads are mandatory wherever the build can install
    // updates itself, so About is purely informational plus the manual
    // check/install actions. A stray toggle here would be a re-introduced
    // setting, not a cosmetic slip.
    respond(supportedStatus);
    render(<AboutSettings />);
    await waitFor(() => expect(screen.getByText('Available')).toBeTruthy());

    expect(screen.queryByRole('checkbox')).toBeNull();
    expect(screen.queryByText(/automatically download/i)).toBeNull();
    expect(mockInvoke).not.toHaveBeenCalledWith('set_auto_update_settings', expect.anything());
  });

  it('keeps the check button and status line for notify-only builds', async () => {
    respond(notifyOnlyStatus);
    render(<AboutSettings />);
    await waitFor(() => expect(screen.getByText('Notify only')).toBeTruthy());

    expect(screen.getByRole('button', { name: /check for updates/i })).toBeTruthy();
    expect(screen.getByText('CLAI is up to date.')).toBeTruthy();
  });

  it('uses the package-manager copy on Linux deb/rpm', async () => {
    respond(linuxPkgStatus);
    render(<AboutSettings />);
    await waitFor(() => expect(screen.getByText('Notify only')).toBeTruthy());

    // Platform-aware body copy: points the user at their package
    // manager instead of a generic "View release" hint.
    expect(
      screen.getByText(
        'CLAI v26.8.1 is available. Use your package manager to install it.'
      )
    ).toBeTruthy();

    // Button label adapts: opening the release-notes page is still
    // useful on Linux deb/rpm (so users can read release notes) but
    // the label drops the misleading "View release" wording.
    expect(screen.getByRole('button', { name: /open release notes/i })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /^view release$/i })).toBeNull();
  });

  it('uses the same package-manager copy on Flatpak (flatpak is the package manager)', async () => {
    respond(linuxFlatpakStatus);
    render(<AboutSettings />);
    await waitFor(() => expect(screen.getByText('Notify only')).toBeTruthy());

    // Same body copy as deb/rpm — Flatpak users invoke
    // `flatpak update` (or the store) just as apt users invoke
    // `apt upgrade`. Both are "package manager" updates from the
    // user's perspective.
    expect(
      screen.getByText(
        'CLAI v26.8.1 is available. Use your package manager to install it.'
      )
    ).toBeTruthy();
    expect(screen.getByRole('button', { name: /open release notes/i })).toBeTruthy();
  });
});
