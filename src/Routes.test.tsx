import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import {
  MemoryRouter,
  Outlet,
  useLocation,
  useNavigate,
  useParams,
} from 'react-router';
import AppRoutes from './Routes';

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

vi.mock('./pages/Workspace', () => ({
  default: () => {
    const { workspaceId } = useParams();
    return <div data-testid="workspace">{workspaceId}</div>;
  },
}));

vi.mock('./pages/NotFound', () => ({
  default: () => <div data-testid="not-found" />,
}));

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

describe('AppRoutes', () => {
  it('sends the root path to the fleet view', () => {
    renderAt('/');
    expect(screen.getByTestId('fleet-index')).toBeInTheDocument();
  });

  it('renders the fleet index nested inside MainLayout > FleetLayout', () => {
    renderAt('/fleet');
    // Nesting, not just presence: FleetLayout owns the persistent workspace
    // rail and must sit inside MainLayout's chrome, not beside it.
    expect(screen.getByTestId('main-layout')).toContainElement(
      screen.getByTestId('fleet-layout')
    );
    expect(screen.getByTestId('fleet-layout')).toContainElement(
      screen.getByTestId('fleet-index')
    );
  });

  it('redirects a bare /workspace with no id back to the fleet, replacing the entry', () => {
    renderEntries(['/somewhere', '/workspace']);
    expect(screen.getByTestId('fleet-index')).toBeInTheDocument();
    expect(screen.queryByTestId('workspace')).not.toBeInTheDocument();

    // Same redirect-loop hazard as the root redirect: without `replace`, Back
    // returns to /workspace and is bounced straight forward again.
    fireEvent.click(screen.getByText('back'));
    expect(screen.getByTestId('pathname')).toHaveTextContent('/somewhere');
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
