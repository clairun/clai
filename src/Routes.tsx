import React from 'react';
import { Routes, Route, Navigate } from 'react-router-dom';
import MainLayout from './layouts/MainLayout';
import NotFound from './pages/NotFound';
import Fleet from './pages/Fleet';
import Workspace from './pages/Workspace';

// The legacy `/` Home page (a tabs/tiles UI from before the workspace model)
// has been deleted; the root path goes straight to Fleet now.
const AppRoutes = () => (
  <Routes>
    <Route element={<MainLayout />}>
      <Route path="/" element={<Navigate to="/fleet" replace />} />
      <Route path="/fleet" element={<Fleet />} />
      <Route path="/workspace" element={<Navigate to="/fleet" replace />} />
      <Route path="/workspace/:workspaceId" element={<Workspace />} />
      <Route path="*" element={<NotFound />} />
    </Route>
  </Routes>
);

export default AppRoutes;
