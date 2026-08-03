import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({
  invoke: mockInvoke,
  // installAppUpdate hands the backend a progress Channel; the badge ignores
  // progress (the package is already downloaded) but the class must exist.
  Channel: class {
    onmessage: unknown = null;
  },
}));

// Capture the app-updates://available handler so tests can fire it.
let listenHandlers: Record<string, (event: { payload: unknown }) => void> = {};
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, handler: (event: { payload: unknown }) => void) => {
    listenHandlers[name] = handler;
    return Promise.resolve(() => {});
  }),
}));

import AppUpdateBadge from './AppUpdateBadge';
import { APP_UPDATE_AVAILABLE_EVENT } from '../utils/appUpdates';
import { OPEN_GLOBAL_SETTINGS_EVENT } from '../utils/globalSettings';

const UPDATE = {
  currentVersion: '26.7.12',
  version: '26.8.1',
  date: null,
  body: null,
  installable: true,
  downloaded: false,
};

const DOWNLOADED = { ...UPDATE, downloaded: true };

const LINUX_DEB_SUPPORT = {
  supported: false,
  canCheck: true,
  platform: 'linux',
  arch: 'x86_64',
  channel: 'linux_pkg',
  bundleType: 'deb',
  reason: 'Notify only: updated by your package manager.',
};

const statusWith = (
  update: typeof UPDATE | null,
  support: Record<string, unknown> = {
    supported: true,
    canCheck: true,
    platform: 'macos',
    arch: 'arm64',
    channel: 'macos',
    bundleType: 'app',
    reason: null,
  }
) => ({
  support,
  lastCheck: update ? { checkedAt: '2026-07-24T12:00:00Z', update, error: null } : null,
});

/** Resolves `get_app_update_status`, and lets each test drive the install. */
const mockBackend = (
  status: ReturnType<typeof statusWith>,
  install?: () => Promise<unknown>
) => {
  mockInvoke.mockImplementation((command: string) => {
    if (command === 'get_app_update_status') return Promise.resolve(status);
    if (command === 'install_app_update') {
      return install ? install() : new Promise(() => {});
    }
    return Promise.reject(new Error(`unexpected command ${command}`));
  });
};

beforeEach(() => {
  mockInvoke.mockReset();
  listenHandlers = {};
});

describe('AppUpdateBadge', () => {
  it('renders nothing when no update is available', async () => {
    mockBackend(statusWith(null));
    const { container } = render(<AppUpdateBadge />);
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_app_update_status'));
    expect(container).toBeEmptyDOMElement();
  });

  it('shows the version from the seeded backend status', async () => {
    mockBackend(statusWith(UPDATE));
    render(<AppUpdateBadge />);
    expect(await screen.findByText(/Update available · v26\.8\.1/)).toBeInTheDocument();
  });

  it('still renders when the build is notify-only (Linux deb/rpm)', async () => {
    // The badge shows the same "Update available" pill for notify-only
    // builds as for self-updating ones — the difference is that no package
    // is ever downloaded, so the restart action never appears.
    mockBackend(statusWith({ ...UPDATE, installable: false }, LINUX_DEB_SUPPORT));
    render(<AppUpdateBadge />);
    expect(await screen.findByText(/Update available · v26\.8\.1/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Restart to install/ })).not.toBeInTheDocument();
  });

  it('appears when an update event fires after mount', async () => {
    mockBackend(statusWith(null));
    render(<AppUpdateBadge />);
    await waitFor(() => expect(listenHandlers[APP_UPDATE_AVAILABLE_EVENT]).toBeDefined());
    listenHandlers[APP_UPDATE_AVAILABLE_EVENT]?.({ payload: { update: UPDATE } });
    expect(await screen.findByText(/Update available · v26\.8\.1/)).toBeInTheDocument();
  });

  it('offers the restart action only once the package is downloaded', async () => {
    mockBackend(statusWith(UPDATE));
    render(<AppUpdateBadge />);
    expect(await screen.findByText(/Update available · v26\.8\.1/)).toBeInTheDocument();
    // While the background download runs there is nothing to restart into,
    // so the pill is informational only.
    expect(screen.queryByRole('button', { name: /Restart to install/ })).not.toBeInTheDocument();

    await waitFor(() => expect(listenHandlers[APP_UPDATE_AVAILABLE_EVENT]).toBeDefined());
    listenHandlers[APP_UPDATE_AVAILABLE_EVENT]?.({ payload: { update: DOWNLOADED } });
    expect(
      await screen.findByRole('button', { name: /Restart to install/ })
    ).toBeInTheDocument();
    // The pill copy stays stable across the flip: the new button is the
    // signal, so the version label never rewrites itself under the cursor.
    expect(screen.getByText(/Update available · v26\.8\.1/)).toBeInTheDocument();
  });

  it('installs and restarts only when the restart action is clicked', async () => {
    mockBackend(statusWith(DOWNLOADED));
    render(<AppUpdateBadge />);
    const action = await screen.findByRole('button', { name: /Restart to install/ });
    expect(mockInvoke).not.toHaveBeenCalledWith('install_app_update', expect.anything());

    await userEvent.click(action);

    expect(mockInvoke).toHaveBeenCalledWith('install_app_update', expect.anything());
    // The backend restarts the app, so the pending state must stay pending
    // rather than inviting a second click into the same install.
    expect(await screen.findByRole('button', { name: /Restarting/ })).toBeDisabled();
  });

  it('surfaces a failed install instead of silently doing nothing', async () => {
    mockBackend(statusWith(DOWNLOADED), () => Promise.reject('Permission denied'));
    render(<AppUpdateBadge />);
    await userEvent.click(await screen.findByRole('button', { name: /Restart to install/ }));

    expect(await screen.findByRole('alert')).toHaveTextContent('Permission denied');
    // Re-enabled: a failed install must be retryable.
    expect(screen.getByRole('button', { name: /Restart to install/ })).toBeEnabled();
  });

  it('clears a failed install when a newer version arrives', async () => {
    mockBackend(statusWith(DOWNLOADED), () => Promise.reject('Permission denied'));
    render(<AppUpdateBadge />);
    await userEvent.click(await screen.findByRole('button', { name: /Restart to install/ }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();

    await waitFor(() => expect(listenHandlers[APP_UPDATE_AVAILABLE_EVENT]).toBeDefined());
    listenHandlers[APP_UPDATE_AVAILABLE_EVENT]?.({
      payload: { update: { ...DOWNLOADED, version: '26.8.2' } },
    });

    expect(await screen.findByText(/Update available · v26\.8\.2/)).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('does not downgrade a downloaded update when a stale event arrives late', async () => {
    mockBackend(statusWith(null));
    render(<AppUpdateBadge />);
    await waitFor(() => expect(listenHandlers[APP_UPDATE_AVAILABLE_EVENT]).toBeDefined());
    listenHandlers[APP_UPDATE_AVAILABLE_EVENT]?.({ payload: { update: DOWNLOADED } });
    expect(
      await screen.findByRole('button', { name: /Restart to install/ })
    ).toBeInTheDocument();
    // A concurrent check that started before the download finished can emit
    // downloaded: false after the fact — the action must not vanish.
    listenHandlers[APP_UPDATE_AVAILABLE_EVENT]?.({ payload: { update: UPDATE } });
    expect(screen.getByRole('button', { name: /Restart to install/ })).toBeInTheDocument();
  });

  it('opens global settings at the About tab on click', async () => {
    mockBackend(statusWith(UPDATE));
    const opened = vi.fn();
    const onOpen = (event: Event) => opened((event as CustomEvent).detail);
    window.addEventListener(OPEN_GLOBAL_SETTINGS_EVENT, onOpen);
    try {
      render(<AppUpdateBadge />);
      const badge = await screen.findByRole('button', { name: /Update available/ });
      await userEvent.click(badge);
      expect(opened).toHaveBeenCalledWith(expect.objectContaining({ tab: 'about' }));
    } finally {
      window.removeEventListener(OPEN_GLOBAL_SETTINGS_EVENT, onOpen);
    }
  });
});
