import React, { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react';
import { useLocation } from 'react-router-dom';
import { getFleetSnapshot } from '../fleet/client';

const FleetContext = createContext(null);
const FLEET_REFRESH_INTERVAL_MS = 5000;

export const useFleet = () => {
  const context = useContext(FleetContext);
  if (!context) {
    throw new Error('useFleet must be used within a FleetProvider');
  }
  return context;
};

export const FleetProvider = ({ children }) => {
  const location = useLocation();
  const isFleetRoute = location.pathname === '/fleet';
  const [snapshot, setSnapshot] = useState(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState(null);
  // `selectedAgent` is now stored directly (not derived from a lookup
  // table). The Fleet page sets it when the user picks a workspace
  // card — to that workspace's default agent — and clears it when the
  // workspace is deselected. Pre-existing global-fleet agent-picking
  // behavior is gone because `fleet_get_snapshot` no longer enumerates
  // agents (they're workspace-local now).
  const [selectedAgent, setSelectedAgent] = useState(null);

  const refresh = useCallback(async () => {
    setIsLoading(true);
    try {
      const nextSnapshot = await getFleetSnapshot();
      setSnapshot(nextSnapshot);
      setError(null);
      return nextSnapshot;
    } catch (err) {
      const message = typeof err === 'string' ? err : (err?.message || 'Failed to load fleet');
      setError(message);
      throw err;
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!isFleetRoute) {
      return undefined;
    }

    let cancelled = false;
    const load = async () => {
      try {
        await refresh();
      } catch {
        if (cancelled) {
          return;
        }
      }
    };

    load();
    const interval = window.setInterval(load, FLEET_REFRESH_INTERVAL_MS);

    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [isFleetRoute, refresh]);

  const value = useMemo(() => ({
    snapshot,
    summary: snapshot?.summary || null,
    agents: snapshot?.agents || [],
    selectedAgentId: selectedAgent?.agentId || null,
    selectedAgent,
    selectAgent: setSelectedAgent,
    isLoading,
    error,
    refresh,
    isFleetRoute,
  }), [snapshot, selectedAgent, isLoading, error, refresh, isFleetRoute]);

  return (
    <FleetContext.Provider value={value}>
      {children}
    </FleetContext.Provider>
  );
};

export default FleetContext;
