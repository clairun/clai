/**
 * Loading a session's chat into the assistant store, and the chat's own
 * actions (older page, queued edit/delete). Shared by the workspace page and
 * the Fleet's selected card so a fix lands once.
 */
import { useMemo } from 'react';
import * as assistantClient from './client';
import useAssistantStore from './sessionStore';
import type { AssistantRun, AssistantSession } from '../generated/bindings';

export const MESSAGE_PAGE_LIMIT = 100;

/**
 * Load the newest message page and the runs (unless given) of `session`.
 * Returns false, without touching the store, when `isCurrent()` turned
 * false meanwhile.
 */
export async function hydrateSession(
  workspaceId: string,
  session: AssistantSession,
  options: { queuedMessageIds?: string[]; runs?: AssistantRun[]; isCurrent?: () => boolean } = {}
): Promise<boolean> {
  const [page, runs] = await Promise.all([
    assistantClient.loadSessionMessagesPage({ workspaceId, sessionId: session.id, limit: MESSAGE_PAGE_LIMIT }),
    options.runs ?? assistantClient.listRuns(workspaceId, session.id),
  ]);
  if (options.isCurrent && !options.isCurrent()) return false;
  useAssistantStore
    .getState()
    .loadSessionData(
      session.id,
      session,
      page.messages,
      runs,
      page.toolCalls,
      options.queuedMessageIds ?? [],
      page.nextCursor ?? null,
      page.hasMore,
      page.totalCount
    );
  return true;
}

export function useSessionChatActions(workspaceId: string, sessionId: string | null) {
  return useMemo(
    () => ({
      // MessageDeleted from the backend removes it from the store; on a lost
      // race ("already picked up") the chip clears via
      // queued_messages_delivered instead, so both outcomes self-resolve.
      onDeleteQueuedMessage: (messageId: string) => {
        if (!sessionId) return;
        assistantClient.deleteQueuedMessage(workspaceId, sessionId, messageId).catch((err) => {
          console.error('[assistant] Failed to delete queued message:', err);
        });
      },
      // AssistantMessageUpdated from the backend swaps the text in the store.
      // On a lost race the edit errors and the original text is left untouched.
      onEditQueuedMessage: async (messageId: string, text: string) => {
        if (!sessionId) return;
        try {
          await assistantClient.editQueuedMessage(workspaceId, sessionId, messageId, text);
        } catch (err) {
          console.error('[assistant] Failed to edit queued message:', err);
        }
      },
      onLoadOlderMessages: () => {
        if (!sessionId) return;
        const store = useAssistantStore.getState();
        const current = store.sessions[sessionId];
        if (!current?.hasOlderMessages || !current.olderMessageCursor || current.isLoadingOlderMessages) {
          return;
        }
        store.setOlderMessagesLoading(sessionId, true);
        assistantClient
          .loadSessionMessagesPage({
            workspaceId,
            sessionId,
            before: current.olderMessageCursor,
            limit: MESSAGE_PAGE_LIMIT,
          })
          .then((page) =>
            useAssistantStore
              .getState()
              .prependMessagePage(sessionId, page.messages, page.toolCalls, page.nextCursor ?? null, page.hasMore, page.totalCount)
          )
          .catch((err) => {
            console.error('[assistant] Failed to load older messages:', err);
            useAssistantStore.getState().setOlderMessagesLoading(sessionId, false);
          });
      },
    }),
    [workspaceId, sessionId]
  );
}
