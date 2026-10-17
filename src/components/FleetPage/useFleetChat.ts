/**
 * Load the selected card's main session into the assistant store, the same
 * way the workspace page does, so the shared chat components render it and
 * the shared terminal knows whether a run is in flight.
 */
import { useEffect, useState } from 'react';
import { assistantClient, useAssistantStore } from '../../assistant';
import { restorePendingAsk } from '../../assistant/pendingAsks';
import { hydrateSession } from '../../assistant/sessionChat';
import { getWorkspaceDetails } from '../../workspace/client';
import type { WorkspaceAgentResponse } from '../../generated/bindings';

const NO_FILES = { includeFiles: false };
const EMPTY_ROSTER: WorkspaceAgentResponse[] = [];

export function useFleetChat(
  workspaceId: string,
  sessionId: string | null
): { roster: readonly WorkspaceAgentResponse[]; error: string | null; loading: boolean } {
  const [roster, setRoster] = useState<WorkspaceAgentResponse[]>(EMPTY_ROSTER);
  // The outcome of the last load, by session; set only from the load's callbacks.
  const [result, setResult] = useState<{ sessionId: string; error: string | null } | null>(null);
  const loaded = useAssistantStore((s) =>
    sessionId ? s.sessions[sessionId]?.totalMessageCount != null : false
  );

  useEffect(() => {
    let cancelled = false;
    getWorkspaceDetails(workspaceId, NO_FILES)
      .then((details) => {
        if (!cancelled) setRoster(details.assignedAgents);
      })
      .catch(() => {
        // Faces fall back to generic ones; the chat still works.
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId]);

  useEffect(() => {
    if (!sessionId) return undefined;
    const store = useAssistantStore.getState();
    store.setActiveSessionForTab(`workspace:${workspaceId}`, sessionId);
    const existing = store.sessions[sessionId];
    // Loaded already (totalMessageCount is set only by a full load), or
    // streaming — a re-read mid-stream would drop the message in flight.
    if (existing && (existing.totalMessageCount != null || existing.isStreaming)) return undefined;

    let cancelled = false;
    assistantClient
      .getSession(workspaceId, sessionId)
      .then(async (session) => {
        if (!session || !(await hydrateSession(workspaceId, session, { isCurrent: () => !cancelled }))) return;
        void restorePendingAsk(workspaceId, sessionId);
        setResult({ sessionId, error: null });
      })
      .catch((err) => {
        if (cancelled) return;
        setResult({ sessionId, error: typeof err === 'string' ? err : 'Failed to load the conversation.' });
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId, sessionId]);

  const current = sessionId && result?.sessionId === sessionId ? result : null;
  return {
    roster,
    error: current?.error ?? null,
    loading: !!sessionId && !loaded && !current,
  };
}
