/**
 * Load the stage card's main session into the assistant store, the same way
 * the workspace page does, so the stage has Main's full last message and the
 * shared terminal knows whether a run is in flight; and its whole task
 * history, which the snapshot cuts to the recent tasks.
 */
import { useEffect, useState } from 'react';
import { assistantClient, useAssistantStore } from '../../assistant';
import { restorePendingAsk } from '../../assistant/pendingAsks';
import { hydrateSession } from '../../assistant/sessionChat';
import { getWorkspaceDetails } from '../../workspace/client';
import { useTaskStore } from '../../workspace/taskStore';

const NO_FILES = { includeFiles: false };

export function useFleetChat(
  workspaceId: string,
  sessionId: string | null
): { error: string | null; loading: boolean } {
  // The outcome of the last load, by session; set only from the load's callbacks.
  const [result, setResult] = useState<{ sessionId: string; error: string | null } | null>(null);
  const loaded = useAssistantStore((s) =>
    sessionId ? s.sessions[sessionId]?.totalMessageCount != null : false
  );

  useEffect(() => {
    let cancelled = false;
    getWorkspaceDetails(workspaceId, NO_FILES)
      .then((details) => {
        if (!cancelled) useTaskStore.getState().seedTasks(workspaceId, details.tasks);
      })
      .catch(() => {
        // The stage shows the tasks the snapshot had.
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
        // A deleted session has nothing to show: settle as empty instead of loading forever.
        if (session) {
          if (!(await hydrateSession(workspaceId, session, { isCurrent: () => !cancelled }))) return;
          void restorePendingAsk(workspaceId, sessionId);
        }
        if (!cancelled) setResult({ sessionId, error: null });
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
    error: current?.error ?? null,
    loading: !!sessionId && !loaded && !current,
  };
}
