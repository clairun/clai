import useAssistantStore from './sessionStore';
import { listPendingAsks } from './client';

const pendingFor = async (workspaceId: string, sessionId: string) =>
  (await listPendingAsks(workspaceId)).find((a) => a.sessionId === sessionId);

/**
 * Re-arm the answer panel for a question asked before this view loaded the
 * session (e.g. after a webview reload). Call after the session is in the store.
 *
 * A question that arrived live meanwhile is newer and is kept. One answered
 * meanwhile left no trace (its clear found no panel), so the list is read
 * again after re-arming; any later answer clears the panel itself.
 */
export async function restorePendingAsk(workspaceId: string, sessionId: string): Promise<void> {
  try {
    const ask = await pendingFor(workspaceId, sessionId);
    const store = useAssistantStore.getState();
    if (!ask || store.sessions[sessionId]?.pendingAskUser) return;
    store.setAskUserPending(sessionId, {
      pendingId: ask.pendingId,
      question: ask.question,
      options: ask.options ?? null,
      multiSelect: ask.multiSelect,
      extraContext: ask.extraContext ?? null,
    });
    if ((await pendingFor(workspaceId, sessionId))?.pendingId !== ask.pendingId) {
      useAssistantStore.getState().clearAskUserPending(sessionId, ask.pendingId);
    }
  } catch (err) {
    console.error('[assistant] Failed to load pending questions:', err);
  }
}
