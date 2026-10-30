import React, { useEffect } from 'react';
import { Routes, Route, Navigate, matchPath, useLocation, useNavigate } from 'react-router';
import MainLayout from './layouts/MainLayout';
import FleetLayout from './layouts/FleetLayout';
import NotFound from './pages/NotFound';
import FleetIndex from './pages/FleetIndex';
import Workspace from './pages/Workspace';
import { listWorkspaces } from './workspace/client';

import FleetPage from './pages/FleetPage';

// The app opens where the user last was: the Fleet (`/fleet`) or a workspace
// (`/workspace/:id`). A remembered workspace that no longer exists, or nothing
// remembered yet, opens the Fleet. The Fleet shows one card per recently
// active workspace, no rail. The workspaces page is a master-detail view:
// `FleetLayout` renders the persistent workspace rail plus an `<Outlet>`.
// `/workspace` auto-selects the most-recent workspace (FleetIndex), and
// `/workspace/:id` renders the full Workspace view in the same shell, so
// the rail stays mounted across navigations.
const LAST_VIEW_KEY = 'clai.lastView';
const FLEET_PATH = '/fleet';
const WORKSPACE_PATH = '/workspace/:workspaceId';

const RememberView = () => {
  const { pathname } = useLocation();
  useEffect(() => {
    if (pathname !== FLEET_PATH && !matchPath(WORKSPACE_PATH, pathname)) return;
    try {
      localStorage.setItem(LAST_VIEW_KEY, pathname);
    } catch {
      // Not remembered; the next launch opens the Fleet.
    }
  }, [pathname]);
  return null;
};

const ReopenWorkspace = ({ workspaceId }: { workspaceId: string }) => {
  const navigate = useNavigate();
  useEffect(() => {
    let cancelled = false;
    listWorkspaces()
      .then(
        (all) => all.some((ws) => ws.id === workspaceId),
        () => false,
      )
      .then((exists) => {
        if (!cancelled) navigate(exists ? `/workspace/${workspaceId}` : FLEET_PATH, { replace: true });
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId, navigate]);
  return null;
};

const StartView = () => {
  let lastView: string | null = null;
  try {
    lastView = localStorage.getItem(LAST_VIEW_KEY);
  } catch {
    // Nothing remembered.
  }
  const workspaceId = lastView ? matchPath(WORKSPACE_PATH, lastView)?.params.workspaceId : undefined;
  return workspaceId ? (
    <ReopenWorkspace workspaceId={workspaceId} />
  ) : (
    <Navigate to={FLEET_PATH} replace />
  );
};

const AppRoutes = () => (
  <>
    <RememberView />
    <Routes>
      <Route element={<MainLayout />}>
        <Route path="/" element={<StartView />} />
        <Route path={FLEET_PATH} element={<FleetPage />} />
        <Route element={<FleetLayout />}>
          <Route path="/workspace" element={<FleetIndex />} />
          <Route path={WORKSPACE_PATH} element={<Workspace />} />
        </Route>
        <Route path="*" element={<NotFound />} />
      </Route>
    </Routes>
  </>
);

export default AppRoutes;
