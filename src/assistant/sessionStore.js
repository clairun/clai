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
  // `null` when the agent isn't currently asking. While set, the
  // chat renders an inline answer block: { pendingId, question,
  // options?, extraContext? }. The pendingId is the round-trip key
  // back to the blocking `ask_user` tool via the
  // `assistant_submit_user_input` Tauri command.
  pendingAskUser: null,
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

      // Mid-turn content swap: replaces the message's persisted content
      // (Claude Code emits tool_use parts as the run progresses, so we
      // flush them to the assistant message immediately for live
      // rendering). Also clears `streamingText` for the message — by
      // the time the backend flushes, the prior text block is closed
      // and its content has been persisted into message.content; the
      // streamingText accumulator must reset so subsequent deltas for
      // the *next* text block don't render alongside the persisted
      // text and double the visible content. Unlike `completeMessage`,
      // this does NOT mark the message as complete (the run keeps
      // streaming).
      updateMessageContent: (sessionId, message) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          const idx = s.messages.findIndex((m) => m.id === message.id);
          if (idx >= 0) {
            s.messages[idx] = message;
          }
          delete s.streamingTextByMessageId[message.id];
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

      setAskUserPending: (sessionId, request) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          s.pendingAskUser = request;
        }),

      clearAskUserPending: (sessionId, pendingId) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          // Only clear if the pending id matches — guards against an
          // out-of-order resolve event for an older request landing after
          // a newer request was already raised.
          if (!s.pendingAskUser) return;
          if (pendingId && s.pendingAskUser.pendingId !== pendingId) return;
          s.pendingAskUser = null;
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
            // Same rationale for the pending ask_user request: it's
            // FE-only state that the BE snapshot doesn't carry, so a
            // poll tick landing while a question is open would unmount
            // AskUserPanel within ~5s and the user could never reach the
            // textarea/options. Cleared on ask_user_resolved.
            pendingAskUser: existing?.pendingAskUser || null,
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
