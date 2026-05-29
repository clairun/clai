import React from 'react';
import { Routes, Route, Navigate } from 'react-router-dom';
import MainLayout from './layouts/MainLayout';
import FleetLayout from './layouts/FleetLayout';
import NotFound from './pages/NotFound';
import FleetIndex from './pages/FleetIndex';
import Workspace from './pages/Workspace';

// The legacy `/` Home page (a tabs/tiles UI from before the workspace model)
// has been deleted; the root path goes straight to Fleet now.
//
// Fleet and Workspace are now one master-detail view: `FleetLayout`
// renders the persistent workspace rail plus an `<Outlet>`. `/fleet`
// auto-selects the most-recent workspace (FleetIndex), and
// `/workspace/:id` renders the full Workspace view in the same shell, so
// the rail stays mounted across navigations.
const AppRoutes = () => (
  <Routes>
    <Route element={<MainLayout />}>
      <Route path="/" element={<Navigate to="/fleet" replace />} />
      <Route element={<FleetLayout />}>
        <Route path="/fleet" element={<FleetIndex />} />
        <Route path="/workspace" element={<Navigate to="/fleet" replace />} />
        <Route path="/workspace/:workspaceId" element={<Workspace />} />
      </Route>
      <Route path="*" element={<NotFound />} />
    </Route>
  </Routes>
);

export default AppRoutes;
