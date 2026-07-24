import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

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

const statusWith = (update: typeof UPDATE | null) => ({
  settings: { autoDownload: true },
  support: {
    supported: true,
    canCheck: true,
    platform: 'linux',
    arch: 'x86_64',
    channel: 'deb',
    bundleType: 'deb',
    reason: null,
  },
  lastCheck: update ? { checkedAt: '2026-07-24T12:00:00Z', update, error: null } : null,
});

beforeEach(() => {
  mockInvoke.mockReset();
  listenHandlers = {};
});

describe('AppUpdateBadge', () => {
  it('renders nothing when no update is available', async () => {
    mockInvoke.mockResolvedValue(statusWith(null));
    const { container } = render(<AppUpdateBadge />);
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('get_app_update_status'));
    expect(container).toBeEmptyDOMElement();
  });

  it('shows the version from the seeded backend status', async () => {
    mockInvoke.mockResolvedValue(statusWith(UPDATE));
    render(<AppUpdateBadge />);
    expect(await screen.findByText(/Update available · v26\.8\.1/)).toBeInTheDocument();
  });

  it('appears when an update event fires after mount', async () => {
    mockInvoke.mockResolvedValue(statusWith(null));
    render(<AppUpdateBadge />);
    await waitFor(() => expect(listenHandlers[APP_UPDATE_AVAILABLE_EVENT]).toBeDefined());
    listenHandlers[APP_UPDATE_AVAILABLE_EVENT]?.({ payload: { update: UPDATE } });
    expect(await screen.findByText(/Update available · v26\.8\.1/)).toBeInTheDocument();
  });

  it('flips to "Update ready" once the package is downloaded', async () => {
    mockInvoke.mockResolvedValue(statusWith(UPDATE));
    render(<AppUpdateBadge />);
    expect(await screen.findByText(/Update available · v26\.8\.1/)).toBeInTheDocument();
    await waitFor(() => expect(listenHandlers[APP_UPDATE_AVAILABLE_EVENT]).toBeDefined());
    listenHandlers[APP_UPDATE_AVAILABLE_EVENT]?.({
      payload: { update: { ...UPDATE, downloaded: true } },
    });
    expect(await screen.findByText(/Update ready · v26\.8\.1/)).toBeInTheDocument();
  });

  it('does not downgrade "Update ready" when a stale event arrives late', async () => {
    mockInvoke.mockResolvedValue(statusWith(null));
    render(<AppUpdateBadge />);
    await waitFor(() => expect(listenHandlers[APP_UPDATE_AVAILABLE_EVENT]).toBeDefined());
    listenHandlers[APP_UPDATE_AVAILABLE_EVENT]?.({
      payload: { update: { ...UPDATE, downloaded: true } },
    });
    expect(await screen.findByText(/Update ready · v26\.8\.1/)).toBeInTheDocument();
    // A concurrent check that started before the download finished can emit
    // downloaded: false after the fact — the badge must not regress.
    listenHandlers[APP_UPDATE_AVAILABLE_EVENT]?.({ payload: { update: UPDATE } });
    expect(await screen.findByText(/Update ready · v26\.8\.1/)).toBeInTheDocument();
  });

  it('opens global settings at the About tab on click', async () => {
    mockInvoke.mockResolvedValue(statusWith(UPDATE));
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
