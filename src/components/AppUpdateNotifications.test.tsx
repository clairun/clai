import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

let listenHandlers: Record<string, (event: { payload: unknown }) => void> = {};
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, handler: (event: { payload: unknown }) => void) => {
    listenHandlers[name] = handler;
    return Promise.resolve(() => {});
  }),
}));

const mockInstallAppUpdate = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
// The toast relies on these utility wrappers; mock them so the install
// path never actually shells out (not that any test triggers it).
vi.mock('../utils/appUpdates', async () => {
  const actual = await vi.importActual<typeof import('../utils/appUpdates')>('../utils/appUpdates');
  return {
    ...actual,
    installAppUpdate: mockInstallAppUpdate,
  };
});
vi.mock('../utils/openExternal', () => ({
  openExternal: vi.fn().mockResolvedValue(undefined),
}));

import AppUpdateNotifications from './AppUpdateNotifications';

const UPDATE = {
  currentVersion: '26.7.12',
  version: '26.8.1',
  date: null,
  body: null,
  installable: true,
  downloaded: false,
};

const macosSupport = {
  supported: true,
  canCheck: true,
  platform: 'macos',
  arch: 'arm64',
  channel: 'macos',
  bundleType: 'app',
  reason: null,
};

const linuxPkgSupport = {
  supported: false,
  canCheck: true,
  platform: 'linux',
  arch: 'x86_64',
  channel: 'linux_pkg',
  bundleType: 'deb',
  reason: 'This Linux install should be updated by its package manager.',
};

const linuxFlatpakSupport = {
  ...linuxPkgSupport,
  bundleType: 'flatpak',
  channel: 'flatpak',
  reason: 'Flatpak updates are managed outside CLAI.',
};

const statusWith = (support: Record<string, unknown>) => ({
  settings: { autoDownload: true },
  support,
  lastCheck: { checkedAt: '2026-07-24T12:00:00Z', update: UPDATE, error: null },
});

beforeEach(() => {
  mockInvoke.mockReset();
  listenHandlers = {};
  mockInstallAppUpdate.mockClear();
});

describe('AppUpdateNotifications (toast)', () => {
  it('renders the install toast on macOS', async () => {
    mockInvoke.mockResolvedValue(statusWith(macosSupport));
    render(<AppUpdateNotifications />);
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_app_update_status'));
    expect(await screen.findByText(/Update available/i)).toBeInTheDocument();
    expect(screen.getByText(/Install and restart/)).toBeInTheDocument();
  });

  it('is suppressed entirely on Linux deb/rpm builds (notify only)', async () => {
    // Phase 6: the toast competes with the user's focused workspace and
    // CLAI cannot install the update itself — the badge + About panel
    // remain the single source of truth.
    mockInvoke.mockResolvedValue(statusWith(linuxPkgSupport));
    const { container } = render(<AppUpdateNotifications />);
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_app_update_status'));
    // Give any settling microtasks a chance: nothing should appear.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(container).toBeEmptyDOMElement();
    expect(screen.queryByText(/Update available/i)).toBeNull();
    expect(screen.queryByText(/Install and restart/)).toBeNull();
  });

  it('is suppressed on Flatpak even though it advertises canCheck', async () => {
    // Flatpak was already notify-only before Phase 6; confirm the new
    // platform-level guard keeps it suppressed.
    mockInvoke.mockResolvedValue(statusWith(linuxFlatpakSupport));
    const { container } = render(<AppUpdateNotifications />);
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_app_update_status'));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(container).toBeEmptyDOMElement();
  });
});
