import { afterEach, beforeEach, describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import {
  MemoryRouter,
  Outlet,
  useLocation,
  useNavigate,
  useParams,
} from 'react-router';
import AppRoutes from './Routes';
import { listWorkspaces } from './workspace/client';
import type { WorkspaceListEntry } from './generated/bindings';

// The real layouts and pages pull in the workspace stores, Tauri `invoke`, and
// the xterm terminal. None of that is the subject here: this file pins the
// ROUTE TABLE in src/Routes.tsx — which path renders which element, and which
// paths redirect. That contract is exactly what a react-router major upgrade
// can silently change (v7 -> v8 dropped the `react-router-dom` package), and
// nothing else in the suite renders <AppRoutes />.
//
// The layout stubs still render <Outlet />, so nested-route resolution is
// genuinely exercised rather than short-circuited.
vi.mock('./layouts/MainLayout', () => ({
  default: () => (
    <div data-testid="main-layout">
      <Outlet />
    </div>
  ),
}));

vi.mock('./layouts/FleetLayout', () => ({
  default: () => (
    <div data-testid="fleet-layout">
      <Outlet />
    </div>
  ),
}));

vi.mock('./pages/FleetIndex', () => ({
  default: () => <div data-testid="fleet-index" />,
}));

vi.mock('./pages/FleetPage', () => ({
  default: () => <div data-testid="fleet-page" />,
}));

vi.mock('./pages/Workspace', () => ({
  default: function WorkspaceStub() {
    const { workspaceId } = useParams();
    return <div data-testid="workspace">{workspaceId}</div>;
  },
}));

vi.mock('./pages/NotFound', () => ({
  default: () => <div data-testid="not-found" />,
}));

vi.mock('./workspace/client', () => ({ listWorkspaces: vi.fn() }));

/** A `localStorage` of its own: Node's global one, without a file, has none. */
const memoryStorage = () => {
  const items = new Map<string, string>();
  return { getItem: (key: string) => items.get(key) ?? null, setItem: (key: string, value: string) => void items.set(key, value) };
};

const existingWorkspaces = (...ids: string[]) =>
  vi.mocked(listWorkspaces).mockResolvedValue(ids.map((id) => ({ id }) as WorkspaceListEntry));

// Reports where the router ended up and can walk history back, so the tests
// can distinguish a redirect that REPLACED the entry from one that pushed.
const HistoryProbe = () => {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  return (
    <>
      <span data-testid="pathname">{pathname}</span>
      <button type="button" onClick={() => navigate(-1)}>
        back
      </button>
    </>
  );
};

const renderEntries = (entries: string[]) =>
  render(
    <MemoryRouter initialEntries={entries} initialIndex={entries.length - 1}>
      <HistoryProbe />
      <AppRoutes />
    </MemoryRouter>
  );

const renderAt = (path: string) => renderEntries([path]);

// One app session that ends on `path`.
const visit = (path: string) => renderAt(path).unmount();

describe('AppRoutes', () => {
  beforeEach(() => {
    vi.stubGlobal('localStorage', memoryStorage());
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('opens on the fleet page on first launch', () => {
    renderAt('/');
    expect(screen.getByTestId('fleet-page')).toBeInTheDocument();
  });

  it('reopens in the workspace the user was last in', async () => {
    existingWorkspaces('ws-1', 'ws-123');
    visit('/workspace/ws-123');

    renderAt('/');
    expect(await screen.findByTestId('workspace')).toHaveTextContent('ws-123');
    expect(screen.getByTestId('pathname')).toHaveTextContent('/workspace/ws-123');
  });

  it('reopens on the fleet when the user left a workspace for the fleet', () => {
    visit('/workspace/ws-123');
    visit('/fleet');

    renderAt('/');
    expect(screen.getByTestId('fleet-page')).toBeInTheDocument();
    expect(listWorkspaces).not.toHaveBeenCalled();
  });

  it('opens the fleet when the last workspace was deleted', async () => {
    existingWorkspaces('ws-1');
    visit('/workspace/ws-123');

    renderAt('/');
    expect(await screen.findByTestId('fleet-page')).toBeInTheDocument();
    expect(screen.queryByTestId('workspace')).not.toBeInTheDocument();
  });

  it('opens the fleet when the workspaces cannot be listed', async () => {
    vi.mocked(listWorkspaces).mockRejectedValue(new Error('backend down'));
    visit('/workspace/ws-123');

    renderAt('/');
    expect(await screen.findByTestId('fleet-page')).toBeInTheDocument();
  });

  it('renders the fleet page without the workspace rail', () => {
    renderAt('/fleet');
    expect(screen.getByTestId('main-layout')).toContainElement(screen.getByTestId('fleet-page'));
    expect(screen.queryByTestId('fleet-layout')).not.toBeInTheDocument();
  });

  it('renders the workspace index inside the workspaces shell', () => {
    renderAt('/workspace');
    expect(screen.getByTestId('fleet-layout')).toContainElement(
      screen.getByTestId('fleet-index')
    );
  });

  it('renders a workspace by id inside the fleet shell, so the rail stays mounted', () => {
    renderAt('/workspace/ws-123');
    expect(screen.getByTestId('main-layout')).toContainElement(
      screen.getByTestId('fleet-layout')
    );
    expect(screen.getByTestId('fleet-layout')).toContainElement(
      screen.getByTestId('workspace')
    );
    expect(screen.getByTestId('workspace')).toHaveTextContent('ws-123');
  });

  it('replaces the redirected entry so Back leaves the app instead of looping', () => {
    // Without `replace`, `/` pushes `/fleet`; Back then returns to `/`, which
    // redirects forward again and the user can never navigate out.
    renderEntries(['/somewhere', '/']);
    expect(screen.getByTestId('pathname')).toHaveTextContent('/fleet');

    fireEvent.click(screen.getByText('back'));
    expect(screen.getByTestId('pathname')).toHaveTextContent('/somewhere');
  });

  it('falls back to NotFound for an unknown path, without the fleet shell', () => {
    renderAt('/nope/not-a-route');
    expect(screen.getByTestId('main-layout')).toContainElement(
      screen.getByTestId('not-found')
    );
    expect(screen.queryByTestId('fleet-layout')).not.toBeInTheDocument();
  });
});
