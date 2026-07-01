/**
 * Assistant Session Store
 *
 * Zustand store managing assistant session state per session and per tab.
 * Receives updates from the assistant event reducer (useAssistantEvents).
 */

import { create } from 'zustand';
import { devtools } from 'zustand/middleware';
import { immer } from 'zustand/middleware/immer';

import type {
  AssistantMessage,
  AssistantMessageCursor,
  AssistantRun,
  AssistantSession,
  ContentPart,
  ToolInvocation,
} from '../generated/bindings';

// FE-only state that the BE snapshot doesn't carry. Lives on the store
// for the lifetime of an in-flight ask_user question; cleared on
// ask_user_resolved.
export interface PendingAskUser {
  pendingId: string;
  question: string;
  options: { label: string; description?: string | null }[] | null;
  /** When true the user may pick any number of options (checkboxes). */
  multiSelect?: boolean;
  extraContext: string | null;
}

export interface SessionState {
  session: AssistantSession;
  messages: AssistantMessage[];
  runs: AssistantRun[];
  toolCalls: ToolInvocation[];
  /** Per-message accumulator for streaming text deltas. Keyed by message id. */
  streamingTextByMessageId: Record<string, string>;
  /** True while a run is queued/running/waiting_for_tool. Drives the chat activity indicator. */
  isStreaming: boolean;
  /** Epoch ms (client clock) when the current run started running, for the
   * running indicator's elapsed-time readout. Null when no run is active. */
  runStartedAt: number | null;
  /** Non-null while the agent is blocked on an ask_user question. */
  pendingAskUser: PendingAskUser | null;
  /** Ids of user messages still waiting in the queue (written while a run
   *  was active, not yet picked up). Rendered with a "Queued" chip; cleared
   *  by `queued_messages_delivered` / `message_deleted` events. */
  queuedMessageIds: string[];
  /** Cursor for loading older messages. Can point at an ancestor session. */
  olderMessageCursor: AssistantMessageCursor | null;
  hasOlderMessages: boolean;
  isLoadingOlderMessages: boolean;
  /** Total messages in the whole conversation (session + rotation ancestors),
   *  not just the loaded window. Seeded from the backend's page responses and
   *  kept live by add/removeMessage; null until the first page load reports it
   *  (fall back to messages.length). */
  totalMessageCount: number | null;
}

export interface AssistantStoreState {
  sessions: Record<string, SessionState>;
  activeSessionByTab: Record<string, string>;

  setActiveSessionForTab: (tabId: string, sessionId: string) => void;
  getActiveSessionForTab: (tabId: string) => string | null;

  initSession: (session: AssistantSession & { tabId?: string | null }) => void;
  addMessage: (sessionId: string, message: AssistantMessage) => void;
  removeMessage: (sessionId: string, messageId: string) => void;
  /** Text of a user message whose run failed and was retracted by the
   *  backend (429/400/token-limit/spawn error), keyed by session. The
   *  composer reads this back into the input box so the typed prompt
   *  isn't lost. Cleared once the composer consumes it. */
  recoverablePrompts: Record<string, string>;
  clearRecoverablePrompt: (sessionId: string) => void;
  markMessageQueued: (sessionId: string, messageId: string) => void;
  markQueuedMessagesDelivered: (sessionId: string, messageIds: string[]) => void;
  prependMessagePage: (
    sessionId: string,
    messages: AssistantMessage[],
    toolCalls: ToolInvocation[],
    cursor: AssistantMessageCursor | null | undefined,
    hasMore: boolean,
    totalCount?: number,
  ) => void;
  setOlderMessagesLoading: (sessionId: string, loading: boolean) => void;
  appendDelta: (sessionId: string, messageId: string, text: string) => void;
  completeMessage: (sessionId: string, message: AssistantMessage) => void;
  updateMessageContent: (sessionId: string, message: AssistantMessage) => void;
  setRunStatus: (sessionId: string, run: AssistantRun) => void;
  setAskUserPending: (sessionId: string, request: PendingAskUser) => void;
  clearAskUserPending: (sessionId: string, pendingId?: string | null) => void;
  setToolCall: (sessionId: string, toolCall: ToolInvocation) => void;
  loadSessionData: (
    sessionId: string,
    session: AssistantSession,
    messages: AssistantMessage[],
    runs?: AssistantRun[],
    toolCalls?: ToolInvocation[],
    queuedMessageIds?: string[],
    olderMessageCursor?: AssistantMessageCursor | null,
    hasOlderMessages?: boolean,
    totalMessageCount?: number | null,
  ) => void;
  removeSession: (sessionId: string) => void;
}

const createInitialSessionState = (session: AssistantSession): SessionState => ({
  session,
  messages: [],
  runs: [],
  toolCalls: [],
  streamingTextByMessageId: {},
  isStreaming: false,
  runStartedAt: null,
  pendingAskUser: null,
  queuedMessageIds: [],
  olderMessageCursor: null,
  hasOlderMessages: false,
  isLoadingOlderMessages: false,
  totalMessageCount: null,
});

const TERMINAL_STATUSES = ['completed', 'completed_with_warnings', 'failed', 'cancelled'] as const;
const ACTIVE_STATUSES = ['queued', 'running', 'waiting_for_tool'] as const;

const useAssistantStore = create<AssistantStoreState>()(
  devtools(
    immer((set, get) => ({
      sessions: {},
      activeSessionByTab: {},
      recoverablePrompts: {},

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
          if (session.tabId) {
            state.activeSessionByTab[session.tabId] = session.id;
          }
        }),

      addMessage: (sessionId, message) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (s) {
            if (!s.messages.find((m) => m.id === message.id)) {
              s.messages.push(message);
              // A genuinely new message grows the conversation total; the
              // dedup guard above keeps replays/races from double-counting.
              if (s.totalMessageCount !== null) {
                s.totalMessageCount += 1;
              }
            }
          }
        }),

      // Backend retracted a message — e.g. a user message whose run failed
      // before the provider produced anything (no point showing a message
      // that never got an answer). Also drops any streaming-text remnant
      // keyed by the message.
      removeMessage: (sessionId, messageId) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          // A retracted *user* message means the run failed before producing
          // anything; stash its typed text so the composer can restore it
          // instead of forcing the user to retype the prompt.
          const removed = s.messages.find((m) => m.id === messageId);
          if (removed && removed.role === 'user') {
            const text = removed.content
              .filter((p): p is Extract<ContentPart, { type: 'text' }> => p.type === 'text')
              .map((p) => p.text)
              .join('')
              .trim();
            if (text) {
              state.recoverablePrompts[sessionId] = text;
            }
          }
          const before = s.messages.length;
          s.messages = s.messages.filter((m) => m.id !== messageId);
          if (s.messages.length < before && s.totalMessageCount !== null) {
            s.totalMessageCount = Math.max(0, s.totalMessageCount - 1);
          }
          s.queuedMessageIds = s.queuedMessageIds.filter((id) => id !== messageId);
          delete s.streamingTextByMessageId[messageId];
        }),

      clearRecoverablePrompt: (sessionId) =>
        set((state) => {
          delete state.recoverablePrompts[sessionId];
        }),

      markMessageQueued: (sessionId, messageId) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          if (!s.queuedMessageIds.includes(messageId)) {
            s.queuedMessageIds.push(messageId);
          }
        }),

      markQueuedMessagesDelivered: (sessionId, messageIds) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          const delivered = new Set(messageIds);
          s.queuedMessageIds = s.queuedMessageIds.filter((id) => !delivered.has(id));
        }),

      prependMessagePage: (sessionId, messages, toolCalls, cursor, hasMore, totalCount) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;

          const existingMessageIds = new Set(s.messages.map((message) => message.id));
          const newMessages = messages.filter((message) => !existingMessageIds.has(message.id));
          s.messages = [...newMessages, ...s.messages];

          const existingToolCallIds = new Set(s.toolCalls.map((toolCall) => toolCall.id));
          for (const toolCall of toolCalls) {
            if (!existingToolCallIds.has(toolCall.id)) {
              s.toolCalls.push(toolCall);
              existingToolCallIds.add(toolCall.id);
            }
          }

          s.olderMessageCursor = cursor ?? null;
          s.hasOlderMessages = hasMore;
          s.isLoadingOlderMessages = false;
          // Each page response carries a fresh backend count — adopt it so
          // any drift from missed events self-corrects on every page load.
          if (totalCount !== undefined) {
            s.totalMessageCount = totalCount;
          }
        }),

      setOlderMessagesLoading: (sessionId, loading) =>
        set((state) => {
          const s = state.sessions[sessionId];
          if (!s) return;
          s.isLoadingOlderMessages = loading;
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
      // text and double the visible content.
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
          if ((TERMINAL_STATUSES as readonly string[]).includes(run.status)) {
            s.isStreaming = false;
            s.streamingTextByMessageId = {};
            s.runStartedAt = null;
            s.pendingAskUser = null;
          } else if ((ACTIVE_STATUSES as readonly string[]).includes(run.status)) {
            s.isStreaming = true;
            // Stamp the start the first time this run is seen running, so the
            // elapsed timer measures from the real run start (a fresh run
            // cleared runStartedAt on the previous terminal transition).
            if (run.status === 'running' && s.runStartedAt == null) {
              s.runStartedAt = Date.now();
            }
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

      loadSessionData: (
        sessionId,
        session,
        messages,
        runs = [],
        toolCalls = [],
        queuedMessageIds,
        olderMessageCursor,
        hasOlderMessages,
        totalMessageCount,
      ) =>
        set((state) => {
          const existing = state.sessions[sessionId];
          state.sessions[sessionId] = {
            ...createInitialSessionState(session),
            messages,
            runs,
            toolCalls,
            // The live event-driven set wins once the session is hydrated.
            // A snapshot reflects the queue at fetch time and can be staler
            // than a queued_messages_delivered event that already cleared a
            // chip (snapshot fetched pre-delivery, applied post-event) —
            // re-applying it resurrected the chip for the whole follow-up
            // run, with no later event to clear it. Events are strictly
            // ordered and the listener is app-global, so an existing entry
            // is never behind; snapshot ids only seed the first hydration
            // (app start / evicted session).
            queuedMessageIds: existing
              ? existing.queuedMessageIds
              : queuedMessageIds ?? [],
            olderMessageCursor:
              olderMessageCursor !== undefined
                ? olderMessageCursor
                : existing?.olderMessageCursor ?? null,
            hasOlderMessages:
              hasOlderMessages !== undefined
                ? hasOlderMessages
                : existing?.hasOlderMessages ?? false,
            isLoadingOlderMessages: false,
            totalMessageCount:
              totalMessageCount !== undefined
                ? totalMessageCount
                : existing?.totalMessageCount ?? null,
            // Preserve in-flight streaming state across snapshot refreshes.
            // The DB only persists assistant text at end-of-run, so a poll
            // tick that lands mid-stream would otherwise wipe the deltas the
            // user is watching arrive, making text flicker on and off.
            streamingTextByMessageId: existing?.streamingTextByMessageId || {},
            isStreaming: existing?.isStreaming || false,
            // Run start is FE-only live state the BE snapshot doesn't carry;
            // preserve it so a poll tick mid-run doesn't reset the elapsed timer.
            runStartedAt: existing?.runStartedAt ?? null,
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
          for (const [tabId, sid] of Object.entries(state.activeSessionByTab)) {
            if (sid === sessionId) {
              delete state.activeSessionByTab[tabId];
            }
          }
        }),
    })),
    { name: 'assistant-store' },
  ),
);

export default useAssistantStore;
