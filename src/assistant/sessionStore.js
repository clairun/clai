/**
 * Assistant Session Store
 *
 * Zustand store managing assistant session state per session and per tab.
 * Receives updates from the assistant event reducer (useAssistantEvents).
 */

import { create } from 'zustand';
import { devtools } from 'zustand/middleware';
import { immer } from 'zustand/middleware/immer';

const createInitialSessionState = (session) => ({
  session,
  messages: [],
  runs: [],
  toolCalls: [],
  streamingTextByMessageId: {},
  isStreaming: false,
});

const useAssistantStore = create(
  devtools(
    immer((set, get) => ({
      // Record<sessionId, SessionState>
      sessions: {},
      // Record<tabId, sessionId>
      activeSessionByTab: {},

      setActiveSessionForTab: (tabId, sessionId) =>
        set((state) => {
          state.activeSessionByTab[tabId] = sessionId;
        }),

      getActiveSessionForTab: (tabId) => {
        return get().activeSessionByTab[tabId] || null;
      },

      initSession: (session) =>
        set((state) => {
          if (!state.sessions[session.id]) {
            state.sessions[session.id] = createInitialSessionState(session);
          }
          // Map tab to session if tab_id present
          if (session.tabId) {
            state.activeSessionByTab[session.tabId] = session.id;
          }
        }),

      addMessage: (sessionId, message) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (s) {
            // Avoid duplicates
            if (!s.messages.find((m) => m.id === message.id)) {
              s.messages.push(message);
            }
          }
        }),

      appendDelta: (sessionId, messageId, text) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (s) {
            s.streamingTextByMessageId[messageId] =
              (s.streamingTextByMessageId[messageId] || '') + text;
            s.isStreaming = true;
          }
        }),

      completeMessage: (sessionId, message) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          const idx = s.messages.findIndex((m) => m.id === message.id);
          if (idx >= 0) {
            s.messages[idx] = message;
          }
          delete s.streamingTextByMessageId[message.id];
          // Intentionally do NOT clear `isStreaming` here. A run typically
          // alternates between assistant text turns and tool-execution
          // phases; clearing on every message completion makes the activity
          // indicator blink off during tool execution and between
          // iterations. `isStreaming` is cleared on terminal run states in
          // `setRunStatus`, which is the right boundary for "agent is no
          // longer working."
        }),

      setRunStatus: (sessionId, run) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          const idx = s.runs.findIndex((r) => r.id === run.id);
          if (idx >= 0) {
            s.runs[idx] = run;
          } else {
            s.runs.push(run);
          }
          if (['completed', 'completed_with_warnings', 'failed', 'cancelled'].includes(run.status)) {
            s.isStreaming = false;
            s.streamingTextByMessageId = {};
          } else if (['queued', 'running', 'waiting_for_tool'].includes(run.status)) {
            // Activity indicator should appear the moment the run is
            // queued/started — before the first text delta arrives —
            // so the user gets immediate feedback that work has begun.
            // It also stays on across tool execution and inter-iteration
            // gaps, where no text deltas are flowing.
            s.isStreaming = true;
          }
        }),

      setToolCall: (sessionId, toolCall) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          const idx = s.toolCalls.findIndex((tc) => tc.id === toolCall.id);
          if (idx >= 0) {
            s.toolCalls[idx] = toolCall;
          } else {
            s.toolCalls.push(toolCall);
          }
        }),

      loadSessionData: (sessionId, session, messages, runs = [], toolCalls = []) =>
        set((state) => {
          const existing = state.sessions[sessionId];
          state.sessions[sessionId] = {
            ...createInitialSessionState(session),
            messages,
            runs,
            toolCalls,
            // Preserve in-flight streaming state across snapshot refreshes.
            // The DB only persists assistant text at end-of-run, so a poll
            // tick that lands mid-stream would otherwise wipe the deltas the
            // user is watching arrive, making text flicker on and off.
            // Stale entries get cleared naturally by completeMessage and
            // setRunStatus when the run terminates.
            streamingTextByMessageId: existing?.streamingTextByMessageId || {},
            isStreaming: existing?.isStreaming || false,
          };
        }),

      removeSession: (sessionId) =>
        set((state) => {
          delete state.sessions[sessionId];
          // Clean up tab mappings
          for (const [tabId, sid] of Object.entries(
            state.activeSessionByTab
          )) {
            if (sid === sessionId) {
              delete state.activeSessionByTab[tabId];
            }
          }
        }),
    })),
    { name: 'assistant-store' }
  )
);

export default useAssistantStore;
