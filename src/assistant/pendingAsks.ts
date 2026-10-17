import useAssistantStore from './sessionStore';
import { listPendingAsks } from './client';

/**
 * Re-arm the answer panel for a question asked before this view loaded the
 * session (e.g. after a webview reload). Call after the session is in the store.
 */
export async function restorePendingAsk(workspaceId: string, sessionId: string): Promise<void> {
  try {
    const asks = await listPendingAsks(workspaceId);
    const ask = asks.find((a) => a.sessionId === sessionId);
    if (!ask) return;
    useAssistantStore.getState().setAskUserPending(sessionId, {
      pendingId: ask.pendingId,
      question: ask.question,
      options: ask.options ?? null,
      multiSelect: ask.multiSelect,
      extraContext: ask.extraContext ?? null,
    });
  } catch (err) {
    console.error('[assistant] Failed to load pending questions:', err);
  }
}
