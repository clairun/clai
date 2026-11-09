import React, { useEffect, useRef } from 'react';
import { Routes, Route, Navigate, Link, matchPath, useLocation, useNavigate } from 'react-router';
import MainLayout from './layouts/MainLayout';
import FleetLayout from './layouts/FleetLayout';
import NotFound from './pages/NotFound';
import FleetIndex from './pages/FleetIndex';
import Workspace from './pages/Workspace';
import { listWorkspaces } from './workspace/client';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import { listen } from '@tauri-apps/api/event';
import deletedStyles from './pages/DeletedWorkspace.module.css';

import FleetPage from './pages/FleetPage';

// The app opens where the user last was: the Fleet (`/fleet`) or a workspace
// (`/workspace/:id`). A remembered workspace that no longer exists opens the
// Fleet; nothing remembered yet (first launch) opens the workspaces page,
// whose conversation is easier to start with. The Fleet shows one card per
// recently active workspace, no rail. The workspaces page is a master-detail view:
// `FleetLayout` renders the persistent workspace rail plus an `<Outlet>`.
// `/workspace` auto-selects the most-recent workspace (FleetIndex), and
// `/workspace/:id` renders the full Workspace view in the same shell, so
// the rail stays mounted across navigations.
const LAST_VIEW_KEY = 'clai.lastView';
const FLEET_PATH = '/fleet';
const WORKSPACES_PATH = '/workspace';
const WORKSPACE_PATH = '/workspace/:workspaceId';

const RememberView = () => {
  const { pathname } = useLocation();
  useEffect(() => {
    if (pathname !== FLEET_PATH && !matchPath(WORKSPACE_PATH, pathname)) return;
    const remember = () => {
      if (!document.hasFocus()) return;
      try {
        localStorage.setItem(LAST_VIEW_KEY, pathname);
      } catch {
        // Not remembered; the next launch opens the workspaces page.
      }
    };
    remember();
    window.addEventListener('focus', remember);
    return () => window.removeEventListener('focus', remember);
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
  const { search } = useLocation();
  const requested = new URLSearchParams(search).get('workspace');
  let lastView: string | null = null;
  try {
    lastView = localStorage.getItem(LAST_VIEW_KEY);
  } catch {
    // Nothing remembered.
  }
  const workspaceId = requested || (lastView ? matchPath(WORKSPACE_PATH, lastView)?.params.workspaceId : undefined);
  return workspaceId ? (
    <ReopenWorkspace workspaceId={workspaceId} />
  ) : (
    <Navigate to={lastView ? FLEET_PATH : WORKSPACES_PATH} replace />
  );
};

const WindowRouteEvents = () => {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const pathnameRef = useRef(pathname);
  useEffect(() => { pathnameRef.current = pathname; }, [pathname]);
  const viewUpdates = useRef<Promise<unknown>>(Promise.resolve());
  useEffect(() => {
    const id = matchPath(WORKSPACE_PATH, pathname)?.params.workspaceId;
    const view = id ? { kind: 'workspace', workspaceId: id } : { kind: pathname === FLEET_PATH ? 'fleet' : 'other' };
    viewUpdates.current = viewUpdates.current.then(() => invoke('window_set_view', { view })).catch(console.error);
  }, [pathname]);
  useEffect(() => {
    let active = true;
    const unlisteners: Array<() => void> = [];
    void getCurrentWebviewWindow().listen<{ workspaceId: string }>('window://navigate', (event) => {
      if (event.payload?.workspaceId) navigate(`/workspace/${event.payload.workspaceId}`);
    }).then((unlisten) => { if (active) unlisteners.push(unlisten); else unlisten(); });
    void listen<{ workspaceId: string }>('workspace://deleted', (event) => {
      if (matchPath(WORKSPACE_PATH, pathnameRef.current)?.params.workspaceId === event.payload?.workspaceId) {
        navigate('/workspace-deleted', { replace: true });
      }
    }).then((unlisten) => { if (active) unlisteners.push(unlisten); else unlisten(); });
    return () => { active = false; unlisteners.forEach((unlisten) => unlisten()); };
  }, [navigate]);
  return null;
};

const DeletedWorkspace = () => (
  <main className={deletedStyles.page}>
    <h1>Workspace deleted</h1>
    <p>This workspace has been deleted. This window can still open another workspace.</p>
    <Link to="/workspace">Open workspaces</Link>
  </main>
);

const AppRoutes = () => (
  <>
    <RememberView />
    <WindowRouteEvents />
    <Routes>
      <Route element={<MainLayout />}>
        <Route path="/" element={<StartView />} />
        <Route path={FLEET_PATH} element={<FleetPage />} />
        <Route path="/workspace-deleted" element={<DeletedWorkspace />} />
        <Route element={<FleetLayout />}>
          <Route path={WORKSPACES_PATH} element={<FleetIndex />} />
          <Route path={WORKSPACE_PATH} element={<Workspace />} />
        </Route>
        <Route path="*" element={<NotFound />} />
      </Route>
    </Routes>
  </>
);

export default AppRoutes;
