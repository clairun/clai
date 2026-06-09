/**
 * useAssistantSession Hook
 *
 * High-level hook that provides assistant session management for a tab.
 * Handles lazy session creation, message sending, and state access.
 */

import { useCallback, useEffect, useRef } from 'react';
import useAssistantStore from './sessionStore';
import * as client from './client';
import type { AssistantSession } from '../generated/bindings';

interface EnsureSessionContext {
  mcpServerIds?: string[];
  [key: string]: unknown;
}

const normalizeIdList = (ids: string[] | undefined | null): string[] => [...(ids || [])].sort();

export function useAssistantSession(tabId: string) {
  const sessionId = useAssistantStore(
    (state) => state.activeSessionByTab[tabId]
  );
  const sessionState = useAssistantStore((state) =>
    sessionId ? state.sessions[sessionId] : null
  );

  // Mirror sessionId into a ref so stable callbacks (sendMessage in particular)
  // always see the latest value. Writing `ref.current` directly during render
  // trips the `react-hooks/refs` lint rule, so the mirror lives in an effect
  // keyed on sessionId — every value change re-runs the effect, but the ref
  // assignment is the only side-effect so the cost is negligible.
  const sessionIdRef = useRef(sessionId);
  useEffect(() => {
    sessionIdRef.current = sessionId;
  }, [sessionId]);

  /**
   * Ensure an assistant session exists for this tab.
   * Checks store → DB → creates new if needed.
   * Returns the session ID.
   */
  const ensureSession = useCallback(
    async (context: EnsureSessionContext = {}) => {
      const store = useAssistantStore.getState();

      // Match on MCP selection for interactive tabs.
      // Integration-specific targeting should be explicit in tool params, not hidden in tab context.
      const contextMcpServerIds = normalizeIdList(context.mcpServerIds || []);

      const sessionMatches = (s: AssistantSession) =>
        JSON.stringify(normalizeIdList(s.context?.mcpServerIds || [])) ===
        JSON.stringify(contextMcpServerIds);

      // Check if we already have a matching session for this tab
      const existingId = store.activeSessionByTab[tabId];
      if (existingId && store.sessions[existingId]) {
        const existing = store.sessions[existingId].session;
        if (sessionMatches(existing)) {
          return existingId;
        }
        // Config changed — remove stale mapping, create new session
        store.removeSession(existingId);
      }

      // Check DB for an existing session attached to this tab with matching config
      try {
        const sessions = await client.listSessions(tabId);
        const matching = sessions.find((s) => sessionMatches(s));
        if (matching) {
          const [messagePage, runs] = await Promise.all([
            client.loadSessionMessagesPage({
              sessionId: matching.id,
              limit: 100,
              includeAncestors: true,
            }),
            client.listRuns(matching.id),
          ]);
          store.loadSessionData(
            matching.id,
            matching,
            messagePage.messages,
            runs,
            messagePage.toolCalls,
            undefined,
            messagePage.nextCursor ?? null,
            messagePage.hasMore,
            messagePage.totalCount,
          );
          store.setActiveSessionForTab(tabId, matching.id);
          return matching.id;
        }
      } catch (err) {
        console.warn('[useAssistantSession] Failed to check existing sessions:', err);
      }

      // Create a new session
      const session = await client.createSession({
        tabId,
        context,
      });
      store.initSession(session);
      store.setActiveSessionForTab(tabId, session.id);
      return session.id;
    },
    [tabId]
  );

  /**
   * Send a message in the current session.
   * The engine handles everything — events update the store.
   */
  const sendMessage = useCallback(async (text: string, connectionId: string) => {
    const sid = sessionIdRef.current;
    if (!sid) throw new Error('No active assistant session for this tab');
    return client.sendMessage(sid, text, connectionId);
  }, []);

  /**
   * Clear all assistant sessions attached to this tab.
   * This prevents older context-specific sessions from being restored later.
   */
  const clearSessions = useCallback(async () => {
    const sessions = await client.listSessions(tabId);
    const store = useAssistantStore.getState();

    await Promise.all(sessions.map((session) => client.deleteSession(session.id)));

    sessions.forEach((session) => {
      store.removeSession(session.id);
    });
  }, [tabId]);

  return {
    sessionId,
    sessionState,
    session: sessionState?.session || null,
    messages: sessionState?.messages || [],
    runs: sessionState?.runs || [],
    streamingText: sessionState?.streamingTextByMessageId || {},
    isStreaming: sessionState?.isStreaming || false,
    ensureSession,
    sendMessage,
    clearSessions,
  };
}
