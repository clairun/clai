import React, { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react';
import { useLocation } from 'react-router-dom';
import { getFleetSnapshot } from '../fleet/client';

// No generated binding for the fleet snapshot yet; modelled loosely from
// the fields the Fleet page reads.
interface FleetSnapshot {
  summary?: unknown;
  agents?: unknown[];
  [key: string]: unknown;
}

interface SelectedAgent {
  agentId?: string;
  sessionId?: string | null;
  providerConnectionIds?: string[];
  tabId?: string | null;
  name?: string;
  workspaceId?: string;
  selectedMcpServerIds?: string[];
  execution?: unknown;
  [key: string]: unknown;
}

interface FleetContextValue {
  snapshot: FleetSnapshot | null;
  summary: unknown;
  agents: unknown[];
  selectedAgentId: string | null;
  selectedAgent: SelectedAgent | null;
  selectAgent: React.Dispatch<React.SetStateAction<SelectedAgent | null>>;
  isLoading: boolean;
  error: string | null;
  refresh: () => Promise<FleetSnapshot | null>;
  isFleetRoute: boolean;
}

const FleetContext = createContext<FleetContextValue | null>(null);
const FLEET_REFRESH_INTERVAL_MS = 5000;

export const useFleet = (): FleetContextValue => {
  const context = useContext(FleetContext);
  if (!context) {
    throw new Error('useFleet must be used within a FleetProvider');
  }
  return context;
};

export const FleetProvider = ({ children }: { children: React.ReactNode }) => {
  const location = useLocation();
  const isFleetRoute = location.pathname === '/fleet';
  const [snapshot, setSnapshot] = useState<FleetSnapshot | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // `selectedAgent` is now stored directly (not derived from a lookup
  // table). The Fleet page sets it when the user picks a workspace
  // card — to that workspace's default agent — and clears it when the
  // workspace is deselected. Pre-existing global-fleet agent-picking
  // behavior is gone because `fleet_get_snapshot` no longer enumerates
  // agents (they're workspace-local now).
  const [selectedAgent, setSelectedAgent] = useState<SelectedAgent | null>(null);

  const refresh = useCallback(async (): Promise<FleetSnapshot | null> => {
    setIsLoading(true);
    try {
      const nextSnapshot = (await getFleetSnapshot()) as FleetSnapshot | null;
      setSnapshot(nextSnapshot);
      setError(null);
      return nextSnapshot;
    } catch (err) {
      const message =
        typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to load fleet';
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

  const value = useMemo<FleetContextValue>(() => ({
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
