import React from 'react';
import { Routes, Route, Navigate } from 'react-router';
import MainLayout from './layouts/MainLayout';
import FleetLayout from './layouts/FleetLayout';
import NotFound from './pages/NotFound';
import FleetIndex from './pages/FleetIndex';
import Workspace from './pages/Workspace';

import FleetPage from './pages/FleetPage';

// The app opens on the Fleet (`/fleet`): one card per recently active
// workspace, no rail. The workspaces page is a master-detail view:
// `FleetLayout` renders the persistent workspace rail plus an `<Outlet>`.
// `/workspace` auto-selects the most-recent workspace (FleetIndex), and
// `/workspace/:id` renders the full Workspace view in the same shell, so
// the rail stays mounted across navigations.
const AppRoutes = () => (
  <Routes>
    <Route element={<MainLayout />}>
      <Route path="/" element={<Navigate to="/fleet" replace />} />
      <Route path="/fleet" element={<FleetPage />} />
      <Route element={<FleetLayout />}>
        <Route path="/workspace" element={<FleetIndex />} />
        <Route path="/workspace/:workspaceId" element={<Workspace />} />
      </Route>
      <Route path="*" element={<NotFound />} />
    </Route>
  </Routes>
);

export default AppRoutes;
