import { useEffect, useMemo, useState } from 'react';
import { listen } from '@tauri-apps/api/event';

const ASSISTANT_EVENT_NAME = 'assistant://event';

// Event-type strings the backend emits. Keep in sync with
// AssistantUiEvent in src-tauri/src/assistant/events.rs.
const RUN_START_TYPES = new Set(['run_started', 'run_queued']);
const RUN_END_TYPES = new Set(['run_completed', 'run_failed', 'run_cancelled']);

/**
 * Tracks which workspaces have at least one assistant run currently in
 * flight. Sources its state from the assistant event stream
 * (RunStarted / RunQueued / RunCompleted / RunFailed / RunCancelled),
 * which fires for both scheduled and user-message-driven runs —
 * closing the gap where the Fleet "running" counter only reflected
 * scheduled tasks.
 *
 * Implementation note: tracks Set<runId> per workspace (not a refcount)
 * so duplicate "start" events (e.g., the engine emits both RunQueued
 * AND RunStarted for the same run) collapse to a single membership
 * entry, and a missing event doesn't leave the count permanently
 * drifted. Both behaviours have happened in practice.
 *
 * Returns an object keyed by workspaceId → number of active runs.
 * Callers can treat any positive value as "processing".
 */
export function useFleetActivity() {
  const [activeRunsByWorkspace, setActiveRunsByWorkspace] = useState({});

  useEffect(() => {
    const unlistenPromise = listen(ASSISTANT_EVENT_NAME, (event) => {
      const envelope = event.payload;
      if (!envelope || !envelope.workspaceId || !envelope.runId) return;
      const type = envelope.event?.type;
      if (!type) return;
      const wsId = envelope.workspaceId;
      const runId = envelope.runId;

      if (RUN_START_TYPES.has(type)) {
        setActiveRunsByWorkspace((current) => {
          const existing = current[wsId];
          if (existing && existing.has(runId)) return current;
          const nextSet = new Set(existing || []);
          nextSet.add(runId);
          return { ...current, [wsId]: nextSet };
        });
      } else if (RUN_END_TYPES.has(type)) {
        setActiveRunsByWorkspace((current) => {
          const existing = current[wsId];
          if (!existing || !existing.has(runId)) return current;
          const nextSet = new Set(existing);
          nextSet.delete(runId);
          if (nextSet.size === 0) {
            const { [wsId]: _removed, ...rest } = current;
            return rest;
          }
          return { ...current, [wsId]: nextSet };
        });
      }
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  // Derive {[workspaceId]: count} from the Set map so consumers can
  // keep the previous numeric API (e.g., `counts[wsId] > 0`).
  return useMemo(() => {
    const out = {};
    for (const [wsId, set] of Object.entries(activeRunsByWorkspace)) {
      out[wsId] = set.size;
    }
    return out;
  }, [activeRunsByWorkspace]);
}
