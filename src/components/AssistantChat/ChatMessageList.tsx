/**
 * ChatMessageList Component
 *
 * Shared message rendering used by both AssistantChat (workspace) and Fleet (detail pane).
 * Handles markdown rendering, tool call display, and auto-scrolling.
 */

import React, { useState, useCallback, useEffect, useMemo, memo } from 'react';
import MarkdownMessage from '../Chat/MarkdownMessage';
import StreamingMarkdown from '../Chat/StreamingMarkdown';
import VirtualizedList from '../common/VirtualizedList';
import type {
  AssistantMessage,
  ContentPart,
  RunNotice,
  ToolInvocation,
} from '../../generated/bindings';
import {
  cleanToolName,
  extractMcpText,
  guessLang,
  summarizeToolCall,
  summarizeToolResult,
  toPreviewText,
} from './toolDisplay';
import styles from './AssistantChat.module.css';
import { onScrollChatToBottom } from '../../utils/workspaceUiEvents';

// Tools beyond this count collapse behind a "show N earlier" toggle so a turn
// that fires dozens of tools stays scannable; the most-recent MAX_VISIBLE rows
// stay on screen.
const MAX_VISIBLE_TOOLS = 4;

// Narrowed ContentPart variants — `Extract` pulls the specific shape out
// of the generated discriminated union so `.text` / `.tool_name` etc.
// are accessible after a `type ===` guard.
type TextPart = Extract<ContentPart, { type: 'text' }>;
type ToolUsePart = Extract<ContentPart, { type: 'tool_use' }>;

// A tool_use enriched with the matching ToolInvocation record (status,
// params, result, error) for rendering.
interface EnrichedToolUse {
  toolCallId: string;
  toolName: string;
  arguments?: unknown;
  status: string;
  params?: unknown;
  result?: unknown;
  error?: string | null;
}

type AssistantSegment =
  | { kind: 'text'; text: string; streaming?: boolean }
  | { kind: 'thinking'; text: string }
  | { kind: 'tools'; toolUses: EnrichedToolUse[] };

type RenderItem =
  | { type: 'load-earlier' }
  | { type: 'message'; message: AssistantMessage }
  | {
      type: 'tool-group';
      id: string;
      createdAt: number | bigint;
      messages: AssistantMessage[];
      toolUses: ToolUsePart[];
    };

const EMPTY_STREAMING: Record<string, string> = {};
const EMPTY_TOOL_CALLS: ToolInvocation[] = [];

const formatTimestamp = (timestamp: number | bigint | null | undefined): string => {
  if (!timestamp) return '';
  const date = new Date(Number(timestamp));
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
};

const getTextContent = (message: AssistantMessage): string => {
  if (!message.content || !Array.isArray(message.content)) return '';
  return message.content
    .filter((part): part is TextPart => part.type === 'text')
    .map((part) => part.text)
    .join('');
};

const getToolUses = (message: AssistantMessage): ToolUsePart[] => {
  if (!message.content || !Array.isArray(message.content)) return [];
  return message.content.filter((part): part is ToolUsePart => part.type === 'tool_use');
};

/**
 * Collapsible "thinking" block — renders the model's reasoning_content
 * with a distinct muted/italic style so it doesn't compete with the
 * user-facing response. Collapsed by default; click to expand.
 */
const ThinkingBlock = memo(({ content }: { content: string }) => {
  const [expanded, setExpanded] = useState(false);
  if (!content) return null;
  const preview = content.slice(0, 120).replace(/\s+/g, ' ').trim();
  return (
    <div className={styles.thinkingBlock}>
      <button
        type="button"
        className={styles.thinkingHeader}
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
      >
        <span className={styles.thinkingIcon} aria-hidden="true">
          {'\u{1F4AD}'}
        </span>
        <span className={styles.thinkingLabel}>Thinking</span>
        {!expanded && preview && (
          <span className={styles.thinkingPreview}>
            {preview}
            {content.length > preview.length ? '…' : ''}
          </span>
        )}
        <span className={styles.thinkingChevron} aria-hidden="true">
          {expanded ? '▾' : '▸'}
        </span>
      </button>
      {expanded && <pre className={styles.thinkingBody}>{content}</pre>}
    </div>
  );
});
ThinkingBlock.displayName = 'ThinkingBlock';

/**
 * Walk an assistant message's `content` array (already ordered) and
 * collapse it into render-ready segments, merging consecutive
 * same-type parts. Consecutive `tool_use` parts share one
 * `ToolCallGroup` so a turn that fires 35 tools in a row still renders
 * compactly instead of producing 35 separate cards; consecutive text
 * (or thinking) parts merge into one block. Empty Text parts (the
 * placeholder the assistant message is seeded with before the first
 * delta arrives) are skipped — otherwise an empty bubble would render
 * a phantom empty paragraph above the first real content.
 *
 * The output order mirrors the source order, so callers see exactly
 * the text↔tool interleaving the agent produced.
 */
const groupAssistantContent = (
  content: ContentPart[],
  toolCallsById: Map<string, ToolInvocation> | undefined
): AssistantSegment[] => {
  if (!Array.isArray(content)) return [];
  const segments: AssistantSegment[] = [];
  for (const part of content) {
    if (!part || typeof part !== 'object') continue;
    if (part.type === 'text') {
      const text = part.text || '';
      if (!text) continue;
      const last = segments[segments.length - 1];
      if (last && last.kind === 'text') {
        last.text += text;
      } else {
        segments.push({ kind: 'text', text });
      }
    } else if (part.type === 'thinking') {
      const text = part.text || '';
      if (!text) continue;
      const last = segments[segments.length - 1];
      if (last && last.kind === 'thinking') {
        last.text += text;
      } else {
        segments.push({ kind: 'thinking', text });
      }
    } else if (part.type === 'tool_use') {
      // O(1) Map lookup (vs the old O(N) Array.find). On a 35-tool
      // turn the difference is ~1200 → ~35 ops per render — material
      // on every re-render the chat tree triggers.
      const tc = toolCallsById?.get(part.tool_call_id);
      const enriched = {
        toolCallId: part.tool_call_id,
        toolName: cleanToolName(part.tool_name),
        arguments: part.arguments,
        status: tc?.status || 'running',
        params: tc?.params,
        result: tc?.result,
        error: tc?.error,
      };
      const last = segments[segments.length - 1];
      if (last && last.kind === 'tools') {
        last.toolUses.push(enriched);
      } else {
        segments.push({ kind: 'tools', toolUses: [enriched] });
      }
    }
  }
  return segments;
};

/**
 * Check if an assistant message contains only tool calls (no text).
 */
const isToolOnlyMessage = (message: AssistantMessage): boolean => {
  if (message.role !== 'assistant') return false;
  const text = getTextContent(message);
  const tools = getToolUses(message);
  return !text.trim() && tools.length > 0;
};

/**
 * Whether an assistant message has anything to show: a non-empty text or
 * thinking part, or any other part kind (tool_use etc.). A just-seeded turn
 * message (one empty Text placeholder) has none — rendering it produces a
 * zero-height item, and VirtualizedList can't cache a 0px measurement, so
 * the item holds its ~48px size estimate. That phantom slot is the gap that
 * flashes under the last tool row while the model composes its next tool
 * call. Callers hide such messages until content (or streaming text) lands.
 */
const hasRenderableContent = (message: AssistantMessage): boolean =>
  Array.isArray(message.content) &&
  message.content.some((part) => {
    if (!part || typeof part !== 'object') return false;
    if (part.type === 'text' || part.type === 'thinking') return !!part.text;
    return true;
  });

const isHiddenMessage = (message: AssistantMessage | undefined): boolean => {
  if (!message) return true;
  if (message.role === 'tool') return true;

  if (message.role !== 'user') return false;
  const text = getTextContent(message);
  return (
    !text || text.startsWith('--- New scheduled run at') || text.startsWith('--- Manual run at')
  );
};

/**
 * Group consecutive tool-only assistant messages into merged blocks.
 * Returns an array of render items:
 * - { type: 'message', message } for normal messages
 * - { type: 'tool-group', messages: [...], toolUses: [...] } for merged tool-only turns
 */
const groupMessages = (messages: AssistantMessage[]): RenderItem[] => {
  const result: RenderItem[] = [];
  let i = 0;

  while (i < messages.length) {
    const msg = messages[i]!; // bounded by the while condition

    if (isHiddenMessage(msg)) {
      i++;
      continue;
    }

    if (isToolOnlyMessage(msg)) {
      // Collect consecutive tool-only assistant messages
      const group = [msg];
      let j = i + 1;
      while (j < messages.length) {
        const candidate = messages[j]!;
        // Skip non-rendered messages between assistant turns.
        if (isHiddenMessage(candidate)) {
          j++;
          continue;
        }
        if (isToolOnlyMessage(candidate)) {
          group.push(candidate);
          j++;
        } else {
          break;
        }
      }

      if (group.length > 1) {
        // Merge into a single tool group
        const allToolUses = group.flatMap((m) => getToolUses(m));
        result.push({
          type: 'tool-group',
          id: group.map((m) => m.id).join('-'),
          createdAt: group[0]!.createdAt,
          messages: group,
          toolUses: allToolUses,
        });
      } else {
        // Single tool-only message — render as normal
        result.push({ type: 'message', message: msg });
      }
      i = j;
    } else {
      result.push({ type: 'message', message: msg });
      i++;
    }
  }

  return result;
};

/**
 * Render a tool result value intelligently:
 * - MCP content arrays: render text parts as markdown
 * - Strings: render as markdown (may contain tables, lists, etc.)
 * - Objects/arrays: pretty-print as JSON
 */
const renderToolResult = (result: unknown): React.ReactNode => {
  if (result == null) return null;

  // Try MCP text extraction first (handles envelope objects and content arrays)
  const mcpText = typeof result === 'object' ? extractMcpText(result) : null;
  if (mcpText) {
    return <MarkdownMessage content={mcpText} isStreaming={false} />;
  }

  // String: render as markdown (detect JSON strings)
  if (typeof result === 'string') {
    const trimmed = result.trim();
    if (
      (trimmed.startsWith('{') && trimmed.endsWith('}')) ||
      (trimmed.startsWith('[') && trimmed.endsWith(']'))
    ) {
      try {
        const parsed = JSON.parse(trimmed);
        return (
          <MarkdownMessage
            content={'```json\n' + JSON.stringify(parsed, null, 2) + '\n```'}
            isStreaming={false}
          />
        );
      } catch {
        /* not JSON, render as markdown */
      }
    }
    return <MarkdownMessage content={result} isStreaming={false} />;
  }

  // Object/array without MCP text: pretty-print as JSON
  if (typeof result === 'object') {
    return (
      <MarkdownMessage
        content={'```json\n' + JSON.stringify(result, null, 2) + '\n```'}
        isStreaming={false}
      />
    );
  }

  return <span>{String(result)}</span>;
};

/**
 * Format tool parameters for display.
 * Returns null if params are empty/null.
 */
const formatParams = (params: unknown): string | null => {
  if (!params) return null;
  if (typeof params === 'object' && Object.keys(params).length === 0) return null;
  if (typeof params === 'string') {
    try {
      const parsed = JSON.parse(params);
      if (typeof parsed === 'object' && Object.keys(parsed).length === 0) return null;
      return JSON.stringify(parsed, null, 2);
    } catch {
      return params;
    }
  }
  return JSON.stringify(params, null, 2);
};

const formatElapsed = (ms: number): string => {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(totalSec / 60);
  const seconds = totalSec % 60;
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
};

/**
 * RunningIndicator — the in-flight footer (left-aligned): the Clai mark on a
 * steady constant-speed spin, plus an elapsed timer so it's clear the run is
 * progressing. The timer always advances (even before any output) so it never
 * looks frozen.
 */
const RunningIndicator = memo(({ runStartedAt }: { runStartedAt?: number | null }) => {
  // Tick once a second to advance the elapsed readout. The footer only mounts
  // while streaming, so the interval is short-lived.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);
  const elapsed = runStartedAt != null ? formatElapsed(now - runStartedAt) : null;

  return (
    <div className={styles.runningIndicator}>
      <img src="/icon.svg" alt="Clai" className={styles.runningIcon} />
      {elapsed && <span className={styles.runningMeta}>{elapsed}</span>}
    </div>
  );
});
RunningIndicator.displayName = 'RunningIndicator';

/**
 * ChatMessageList - Renders a list of assistant messages with markdown and tool calls
 */
interface ChatMessageListProps {
  messages: AssistantMessage[];
  streamingText?: Record<string, string>;
  isStreaming?: boolean;
  toolCalls?: ToolInvocation[];
  userLabel?: string;
  // Error message of the most recent run if it failed, shown inline at the
  // end of the conversation (directly under the failed turn). Null when the
  // last run didn't fail. `runErrorIsLimit` selects a calmer style for
  // usage/rate limits, which resolve on their own at a stated reset time.
  runError?: string | null;
  runErrorIsLimit?: boolean;
  // Epoch ms when the in-flight run started, for the running indicator's
  // elapsed-time readout.
  runStartedAt?: number | null;
  // Ids of user messages still waiting in the queue (written while a run
  // was active, not yet picked up). Rendered with a "Queued" chip.
  queuedMessageIds?: string[];
  // Remove a still-queued message before any run picks it up. Omit to
  // hide the remove affordance (e.g. read-only transcript views).
  onDeleteQueuedMessage?: (messageId: string) => void;
  // Edit a still-queued message's text in-place before any run picks it
  // up. Omit to hide the edit affordance.
  onEditQueuedMessage?: (messageId: string, text: string) => Promise<void> | void;
  hasOlderMessages?: boolean;
  isLoadingOlderMessages?: boolean;
  onLoadOlderMessages?: () => void;
}

const ChatMessageList = ({
  messages,
  streamingText = EMPTY_STREAMING,
  isStreaming = false,
  toolCalls = EMPTY_TOOL_CALLS,
  // Label shown for `user`-role messages. Defaults to "You" for the human in
  // the main chat. Pass "Main agent" when rendering a sub-agent's task
  // transcript — those `user` messages are the parent's task instructions,
  // not anything the human typed.
  userLabel = 'You',
  runError = null,
  runErrorIsLimit = false,
  runStartedAt = null,
  queuedMessageIds,
  onDeleteQueuedMessage,
  onEditQueuedMessage,
  hasOlderMessages = false,
  isLoadingOlderMessages = false,
  onLoadOlderMessages,
}: ChatMessageListProps) => {
  // Build a Map of toolCalls keyed by id once per render, so every
  // tool_use part lookup is O(1) instead of an Array.find walk. Memoized
  // on the toolCalls reference so the Map is stable while toolCalls
  // doesn't change, which keeps memoized children from re-rendering.
  const toolCallsById = useMemo(() => {
    const map = new Map<string, ToolInvocation>();
    for (const tc of toolCalls) map.set(tc.id, tc);
    return map;
  }, [toolCalls]);

  // External scroll-to-bottom nudges (e.g. entering terminal mode shrinks the
  // conversation viewport). Folded into scrollToBottomSignal below with a wide
  // multiplier so it never collides with the messages.length component. The
  // underlying VirtualizedList still honours "only if the reader was near the
  // bottom", so a reader scrolled up into history is left undisturbed.
  const [scrollNudge, setScrollNudge] = useState(0);
  useEffect(() => onScrollChatToBottom(() => setScrollNudge((n) => n + 1)), []);

  // Drop assistant messages that have nothing to render yet — the empty
  // placeholder each turn is seeded with stays hidden until its first
  // content part or streaming delta arrives (see hasRenderableContent).
  const visibleMessages = useMemo(
    () =>
      messages.filter(
        (msg) => msg.role !== 'assistant' || hasRenderableContent(msg) || !!streamingText[msg.id]
      ),
    [messages, streamingText]
  );

  const grouped = useMemo(() => {
    const items = groupMessages(visibleMessages);
    if (hasOlderMessages && onLoadOlderMessages) {
      return [{ type: 'load-earlier' } as RenderItem, ...items];
    }
    return items;
  }, [visibleMessages, hasOlderMessages, onLoadOlderMessages]);

  // Id of the last visible message iff it's a user message. Writing a message
  // is an explicit "show me the latest" — when a new user message lands at the
  // tail of the conversation, the list jumps to the bottom even if the reader
  // had scrolled up into history.
  // Scans visibleMessages (not raw messages) so the still-empty assistant
  // placeholder seeded right after a send can't mask the user message when
  // both land in the same render batch.
  const lastUserMessageId = useMemo(() => {
    for (let i = visibleMessages.length - 1; i >= 0; i -= 1) {
      const msg = visibleMessages[i]!;
      if (isHiddenMessage(msg)) continue;
      return msg.role === 'user' ? msg.id : null;
    }
    return null;
  }, [visibleMessages]);

  // An item is an "assistant continuation" when it's an assistant turn
  // (text+tools MessageBlock or a tool-only MergedToolGroup) AND the
  // item before it is also assistant/tool. Continuations render slim:
  // no header, no card chrome, just the content flowing into the
  // previous block. The first assistant after a user message keeps
  // the full header and card framing so the turn boundary stays
  // legible.
  const continuationFlags = useMemo(() => {
    const isAssistantItem = (it: RenderItem | undefined) => {
      if (!it) return false;
      if (it.type === 'load-earlier') return false;
      if (it.type === 'tool-group') return true;
      return it.message?.role === 'assistant';
    };

    return grouped.map(
      (_, idx) => isAssistantItem(grouped[idx]) && isAssistantItem(grouped[idx - 1])
    );
  }, [grouped]);

  const itemKey = useCallback(
    (item: RenderItem) =>
      item.type === 'load-earlier'
        ? 'load-earlier'
        : item.type === 'tool-group'
          ? `tool-group:${item.id}`
          : `message:${item.message.id}`,
    []
  );

  // Set lookup for the queued chip; stable reference while the id list
  // doesn't change so memoized MessageBlocks don't re-render.
  const queuedIdSet = useMemo(() => new Set(queuedMessageIds ?? []), [queuedMessageIds]);

  const renderItem = useCallback(
    (item: RenderItem, idx: number) =>
      item.type === 'load-earlier' ? (
        <div className={styles.loadEarlierWrap}>
          <button
            type="button"
            className={styles.loadEarlierButton}
            onClick={onLoadOlderMessages}
            disabled={isLoadingOlderMessages}
          >
            {isLoadingOlderMessages ? 'Loading…' : 'Load earlier'}
          </button>
        </div>
      ) : item.type === 'tool-group' ? (
        <MergedToolGroup
          item={item}
          toolCallsById={toolCallsById}
          isContinuation={continuationFlags[idx]}
        />
      ) : (
        <MessageBlock
          message={item.message}
          streamingText={streamingText[item.message.id]}
          toolCallsById={toolCallsById}
          userLabel={userLabel}
          isContinuation={continuationFlags[idx]}
          isQueued={queuedIdSet.has(item.message.id)}
          onDeleteQueued={onDeleteQueuedMessage}
          onEditQueued={onEditQueuedMessage}
        />
      ),
    [
      continuationFlags,
      streamingText,
      toolCallsById,
      userLabel,
      queuedIdSet,
      onDeleteQueuedMessage,
      onEditQueuedMessage,
      onLoadOlderMessages,
      isLoadingOlderMessages,
    ]
  );

  // Auto-load older pages as the reader scrolls toward the top.
  // VirtualizedList only fires this on user-initiated upward scrolls (never
  // on its own prepend/pin corrections), and the parent's load handler gates
  // re-entrancy on isLoadingOlderMessages — so repeated fires while a page
  // is in flight are harmless no-ops. The "Load earlier" row stays as a
  // visible affordance/fallback and doubles as the loading indicator.
  const handleApproachTop = hasOlderMessages ? onLoadOlderMessages : undefined;

  // Footer rendered inside the scroll area, right after the last message.
  // While a run is in flight we show the activity indicator; once it ends we
  // show the failure (if any) attached to the turn it belongs to. These are
  // mutually exclusive — a failed run is no longer streaming.
  const footer = isStreaming ? (
    <RunningIndicator runStartedAt={runStartedAt} />
  ) : runError ? (
    <div className={runErrorIsLimit ? styles.runLimitBanner : styles.runErrorBanner} role="alert">
      <span className={styles.runErrorIcon}>{runErrorIsLimit ? '⏳' : '⚠'}</span>
      <span>{runError}</span>
    </div>
  ) : null;

  return (
    <VirtualizedList
      items={grouped}
      itemKey={itemKey}
      renderItem={renderItem}
      className={styles.activityList}
      // Most turns are now one-line tool rows (~30px). A large estimate
      // over-allocates each not-yet-measured row, so during an active run the
      // footer/last row sits well below the real content and stick-to-bottom
      // scrolls into that empty slot — the "jumps off the bottom on every new
      // tool" gap. Estimating near the common row height keeps the transient
      // gap negligible; taller text blocks correct on measure (overscan keeps
      // them rendered/measured).
      estimateSize={48}
      overscan={1400}
      gap={12}
      footer={footer}
      footerEstimateSize={56}
      initialScrollToBottom
      scrollToBottomSignal={messages.length + scrollNudge * 1_000_000}
      scrollToBottomBehavior="auto"
      forceScrollToBottomKey={lastUserMessageId}
      onApproachTop={handleApproachTop}
    />
  );
};

interface MessageBlockProps {
  message: AssistantMessage;
  streamingText?: string;
  toolCallsById: Map<string, ToolInvocation>;
  userLabel?: string;
  isContinuation?: boolean;
  isQueued?: boolean;
  onDeleteQueued?: (messageId: string) => void;
  onEditQueued?: (messageId: string, text: string) => Promise<void> | void;
}

const MessageBlock = memo(
  ({
    message,
    streamingText,
    toolCallsById,
    userLabel = 'You',
    isContinuation = false,
    isQueued = false,
    onDeleteQueued,
    onEditQueued,
  }: MessageBlockProps) => {
    const { role, createdAt } = message;
    // Local draft state for editing a queued message in-place. `null` = not
    // editing; a string = the working draft seeded from the message text.
    const [editDraft, setEditDraft] = useState<string | null>(null);

    if (role === 'user') {
      const textContent = getTextContent(message);
      if (!textContent) return null;

      // Hide run boundary markers (persisted trigger messages for the LLM, not for the user)
      if (
        textContent.startsWith('--- New scheduled run at') ||
        textContent.startsWith('--- Manual run at')
      ) {
        return null;
      }

      const isEditing = editDraft !== null;
      const trimmedDraft = (editDraft ?? '').trim();
      const submitEdit = () => {
        if (!trimmedDraft || trimmedDraft === textContent.trim()) {
          setEditDraft(null);
          return;
        }
        // The backend emits AssistantMessageUpdated, which swaps the text in
        // the store; close the editor optimistically. On error the store is
        // untouched and the original text remains.
        void Promise.resolve(onEditQueued?.(message.id, trimmedDraft)).catch(() => {});
        setEditDraft(null);
      };

      return (
        <div className={`${styles.userMessage} ${isQueued ? styles.userMessageQueued : ''}`}>
          <div className={styles.messageHeader}>
            <span className={styles.messageRoleText}>{userLabel}</span>
            {createdAt && (
              <span className={styles.messageTimestamp}>{formatTimestamp(createdAt)}</span>
            )}
            {isQueued && (
              <span className={styles.queuedChip} title="Waiting for the agent to pick this up">
                Queued
              </span>
            )}
            {isQueued && onEditQueued && !isEditing && (
              <button
                type="button"
                className={styles.queuedEdit}
                onClick={() => setEditDraft(textContent)}
                title="Edit before it's picked up"
                aria-label="Edit queued message"
              >
                ✎
              </button>
            )}
            {isQueued && onDeleteQueued && !isEditing && (
              <button
                type="button"
                className={styles.queuedRemove}
                onClick={() => onDeleteQueued(message.id)}
                title="Remove before it's picked up"
                aria-label="Remove queued message"
              >
                ×
              </button>
            )}
          </div>
          {isEditing ? (
            <div className={styles.queuedEditor}>
              <textarea
                className={styles.queuedEditorInput}
                value={editDraft ?? ''}
                autoFocus
                rows={Math.min(8, Math.max(2, (editDraft ?? '').split('\n').length))}
                onChange={(e) => setEditDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !e.shiftKey) {
                    e.preventDefault();
                    submitEdit();
                  } else if (e.key === 'Escape') {
                    e.preventDefault();
                    setEditDraft(null);
                  }
                }}
              />
              <div className={styles.queuedEditorActions}>
                <button
                  type="button"
                  className={styles.queuedEditorSave}
                  onClick={submitEdit}
                  disabled={!trimmedDraft}
                >
                  Save
                </button>
                <button
                  type="button"
                  className={styles.queuedEditorCancel}
                  onClick={() => setEditDraft(null)}
                >
                  Cancel
                </button>
              </div>
            </div>
          ) : (
            <div className={styles.messageContent}>
              <MarkdownMessage content={textContent} />
            </div>
          )}
        </div>
      );
    }

    if (role === 'assistant') {
      // Walk message.content in order, grouping consecutive same-type
      // parts into segments. This preserves the interleaving (text → tool
      // → text → tool …) the assistant actually produced, rather than
      // collapsing all text to the top and all tools to the bottom.
      // streamingText is appended as a trailing segment because by
      // definition it represents the *current* in-flight text block,
      // which sits after everything already persisted. Memoized so
      // ToolCallGroup's prop reference is stable across re-renders when
      // nothing actually changed — its memo only helps if its toolUses
      // array reference doesn't churn.
      const segments = useMemo(() => {
        const base = groupAssistantContent(message.content, toolCallsById);
        if (streamingText) {
          base.push({ kind: 'text', text: streamingText, streaming: true });
        }
        return base;
      }, [message.content, toolCallsById, streamingText]);

      return (
        <div className={isContinuation ? styles.assistantContinuation : styles.assistantMessage}>
          {!isContinuation && (
            <div className={styles.messageHeader}>
              <span className={styles.messageRoleText}>Clai</span>
              {createdAt && (
                <span className={styles.messageTimestamp}>{formatTimestamp(createdAt)}</span>
              )}
            </div>
          )}
          <div className={styles.messageContent}>
            {segments.map((seg, idx) => {
              if (seg.kind === 'thinking') {
                return <ThinkingBlock key={idx} content={seg.text} />;
              }
              if (seg.kind === 'text') {
                return (
                  <StreamingMarkdown key={idx} content={seg.text} isStreaming={!!seg.streaming} />
                );
              }
              if (seg.kind === 'tools') {
                return <ToolCallGroup key={idx} toolUses={seg.toolUses} />;
              }
              return null;
            })}
          </div>
        </div>
      );
    }

    // Skip tool result messages — shown inline with tool calls
    return null;
  }
);

/**
 * MergedToolGroup — renders tool calls from multiple consecutive assistant turns
 * as a single collapsed group, avoiding repeated "CLAI" headers for tool-only turns.
 */
interface MergedToolGroupProps {
  item: Extract<RenderItem, { type: 'tool-group' }>;
  toolCallsById: Map<string, ToolInvocation>;
  isContinuation?: boolean;
}

const MergedToolGroup = memo(
  ({ item, toolCallsById, isContinuation = false }: MergedToolGroupProps) => {
    const enrichedToolUses = useMemo<EnrichedToolUse[]>(
      () =>
        item.toolUses.map((tu) => {
          const tc = toolCallsById?.get(tu.tool_call_id);
          return {
            toolCallId: tu.tool_call_id,
            toolName: cleanToolName(tu.tool_name),
            arguments: tu.arguments,
            status: tc?.status || 'running',
            params: tc?.params,
            result: tc?.result,
            error: tc?.error,
          };
        }),
      [item.toolUses, toolCallsById]
    );

    return (
      <div className={isContinuation ? styles.assistantContinuation : styles.assistantMessage}>
        {!isContinuation && (
          <div className={styles.messageHeader}>
            <span className={styles.messageRoleText}>Clai</span>
            {item.createdAt && (
              <span className={styles.messageTimestamp}>{formatTimestamp(item.createdAt)}</span>
            )}
          </div>
        )}
        <div className={styles.messageContent}>
          <ToolCallGroup toolUses={enrichedToolUses} />
        </div>
      </div>
    );
  }
);

/** Coerce a value into a plain object (parsing JSON strings) or null. */
const toObj = (value: unknown): Record<string, unknown> | null => {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }
  if (typeof value === 'string') {
    try {
      const parsed = JSON.parse(value);
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        return parsed as Record<string, unknown>;
      }
    } catch {
      /* not JSON */
    }
  }
  return null;
};

/**
 * Render a tool's expanded output, formatted per tool:
 * - bash_exec → the command as a `$ …` line + a terminal-style block (raw,
 *   monospace, whitespace-preserving — markdown would mangle shell output).
 * - fs_read / fs_write → file content in a fenced block with a language
 *   guessed from the path, so it's syntax-highlighted.
 * - anything else (MCP text, JSON, …) → the existing smart `renderToolResult`.
 */
const renderToolOutput = (
  toolName: string,
  params: unknown,
  result: unknown,
  error: string | null | undefined,
  isRunning: boolean
): React.ReactNode => {
  const name = cleanToolName(toolName || '');

  if (isRunning && result == null && !error) {
    return (
      <div className={styles.loadingState}>
        <span className={styles.spinner} />
        <span>Executing…</span>
      </div>
    );
  }

  if (name === 'bash_exec') {
    const command = toObj(params)?.command;
    const body = toPreviewText('bash_exec', result, error);
    return (
      <div className={styles.toolTerminalWrap}>
        {typeof command === 'string' && command && (
          <div className={styles.toolCommand}>{`$ ${command}`}</div>
        )}
        <pre className={`${styles.toolTerminal} ${error ? styles.toolTerminalError : ''}`}>
          {body}
        </pre>
      </div>
    );
  }

  if (name === 'ask_user') {
    const p = toObj(params);
    const r = toObj(result);
    const question = typeof p?.question === 'string' ? p.question : '';
    const context = typeof p?.context === 'string' ? p.context : '';
    const options = Array.isArray(p?.options)
      ? (p!.options as Array<{ label?: string; description?: string | null }>)
      : [];
    const answer = typeof r?.answer === 'string' ? r.answer : '';
    // Highlight what the user picked: single-select answers carry
    // selectedOptionIndex, multi-select answers selectedOptionIndexes.
    const selectedSet = new Set<number>(
      Array.isArray(r?.selectedOptionIndexes)
        ? (r!.selectedOptionIndexes as unknown[]).filter((v): v is number => typeof v === 'number')
        : typeof r?.selectedOptionIndex === 'number'
          ? [r.selectedOptionIndex]
          : []
    );
    const isMultiSelect = p?.multiSelect === true;
    return (
      <div className={styles.askUser}>
        {question && <div className={styles.askUserQuestion}>{question}</div>}
        {context && <MarkdownMessage content={context} isStreaming={false} />}
        {options.length > 0 && (
          <ul className={styles.askUserOptions}>
            {options.map((opt, i) => {
              const label = typeof opt?.label === 'string' ? opt.label : '';
              const selected = selectedSet.has(i);
              return (
                <li
                  key={i}
                  className={`${styles.askUserOption} ${selected ? styles.askUserOptionSelected : ''}`}
                >
                  <span className={styles.askUserBullet}>
                    {isMultiSelect ? (selected ? '☑' : '☐') : selected ? '●' : '○'}
                  </span>
                  <span>
                    {label}
                    {opt?.description ? (
                      <span className={styles.askUserOptionDesc}>{opt.description}</span>
                    ) : null}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
        {answer && (
          <div className={styles.askUserAnswer}>
            <span className={styles.askUserAnswerLabel}>Answer</span>
            <MarkdownMessage content={answer} isStreaming={false} />
          </div>
        )}
      </div>
    );
  }

  if (error) {
    return <pre className={`${styles.toolTerminal} ${styles.toolTerminalError}`}>{error}</pre>;
  }

  if (name === 'fs_read' || name === 'fs_write') {
    const fromResult = toObj(result);
    const fromParams = toObj(params);
    const path =
      (typeof fromResult?.path === 'string' && fromResult.path) ||
      (typeof fromParams?.path === 'string' && fromParams.path) ||
      '';
    const content =
      (typeof fromResult?.content === 'string' && fromResult.content) ||
      (typeof fromParams?.content === 'string' && fromParams.content) ||
      '';
    if (content) {
      return (
        <MarkdownMessage
          content={'```' + guessLang(path) + '\n' + content + '\n```'}
          isStreaming={false}
        />
      );
    }
  }

  return renderToolResult(result);
};

/**
 * ToolCallGroup — renders a turn's tool calls as compact one-line rows.
 * Beyond MAX_VISIBLE_TOOLS, older calls collapse behind a "show N earlier"
 * toggle so a 35-tool turn stays scannable.
 */
const ToolCallGroup = memo(({ toolUses }: { toolUses: EnrichedToolUse[] }) => {
  const [showEarlier, setShowEarlier] = useState(false);

  if (toolUses.length === 0) return null;

  const overflow = toolUses.length - MAX_VISIBLE_TOOLS;
  const hasOverflow = overflow > 0;
  const visible = hasOverflow && !showEarlier ? toolUses.slice(-MAX_VISIBLE_TOOLS) : toolUses;

  return (
    <div className={styles.toolList}>
      {hasOverflow && (
        <button
          type="button"
          className={styles.toolShowEarlier}
          onClick={() => setShowEarlier((prev) => !prev)}
          aria-expanded={showEarlier}
        >
          <span className={`${styles.toolRowChevron} ${showEarlier ? styles.expanded : ''}`}>
            ▸
          </span>
          {showEarlier
            ? 'Hide earlier calls'
            : `Show ${overflow} earlier ${overflow === 1 ? 'call' : 'calls'}`}
        </button>
      )}
      {visible.map((tu) => (
        <ToolRow
          key={tu.toolCallId}
          toolName={tu.toolName}
          params={tu.params ?? tu.arguments}
          status={tu.status}
          result={tu.result}
          error={tu.error}
        />
      ))}
    </div>
  );
});

/**
 * ToolRow — a single tool call as one scannable line:
 *   <status> <verb> <primary arg> ............... <result summary> <chevron>
 * Click to expand the full, well-formatted Output/Input view.
 */
interface ToolRowProps {
  toolName: string;
  params?: unknown;
  status: string;
  result?: unknown;
  error?: string | null;
}

const ToolRow = memo(({ toolName, params, status, result, error }: ToolRowProps) => {
  const [isExpanded, setIsExpanded] = useState(false);
  const [activeTab, setActiveTab] = useState<'output' | 'input'>('output');

  const handleToggle = useCallback(() => setIsExpanded((prev) => !prev), []);

  const { verb, arg } = useMemo(() => summarizeToolCall(toolName, params), [toolName, params]);
  const resultSummary = useMemo(
    () => summarizeToolResult(toolName, result, error, status),
    [toolName, result, error, status]
  );

  const isRunning = status === 'running';
  const isFailed = status === 'failed' || !!error;
  const icon = isFailed ? '✗' : isRunning ? '⚙' : '✓';

  // formatParams hides empty params — right for tools legitimately called
  // with no args, but on a failed call "the model sent {}" is exactly what
  // the user needs to see (e.g. a schema-validation reject for a missing
  // required property), so fall back to the raw JSON there.
  const formattedParams =
    formatParams(params) ?? (isFailed && params != null ? JSON.stringify(params, null, 2) : null);
  const hasInput = !!formattedParams;
  const hasOutput = result != null || !!error || isRunning;

  return (
    <div className={styles.toolRowBlock}>
      <button
        type="button"
        className={styles.toolRow}
        onClick={handleToggle}
        aria-expanded={isExpanded}
      >
        <span
          className={`${styles.toolRowIcon} ${isFailed ? styles.toolRowIconError : ''} ${isRunning ? styles.toolRowIconRunning : ''}`}
        >
          {icon}
        </span>
        <span className={styles.toolRowVerb}>{verb}</span>
        {arg && <span className={styles.toolRowArg}>{arg}</span>}
        <span className={styles.toolRowRight}>
          {isRunning ? (
            <span className={styles.toolRowRunning}>
              <span className={styles.spinner} />
              running…
            </span>
          ) : resultSummary ? (
            <span
              className={`${styles.toolRowSummary} ${resultSummary.tone === 'error' ? styles.toolRowSummaryError : ''}`}
            >
              {resultSummary.text}
            </span>
          ) : null}
          <span className={`${styles.toolRowChevron} ${isExpanded ? styles.expanded : ''}`}>▾</span>
        </span>
      </button>

      {isExpanded && (
        <div className={styles.toolContent}>
          {(hasInput || hasOutput) && (
            <div className={styles.toolTabs}>
              {hasOutput && (
                <button
                  type="button"
                  className={`${styles.toolTab} ${activeTab === 'output' ? styles.toolTabActive : ''}`}
                  onClick={() => setActiveTab('output')}
                >
                  Output
                </button>
              )}
              {hasInput && (
                <button
                  type="button"
                  className={`${styles.toolTab} ${activeTab === 'input' ? styles.toolTabActive : ''}`}
                  onClick={() => setActiveTab('input')}
                >
                  Input
                </button>
              )}
            </div>
          )}

          {activeTab === 'input' && hasInput && (
            <div className={styles.toolResult}>
              <MarkdownMessage
                content={'```json\n' + formattedParams + '\n```'}
                isStreaming={false}
              />
            </div>
          )}

          {activeTab === 'output' && (
            <div className={styles.toolResult}>
              {renderToolOutput(toolName, params, result, error, isRunning)}
            </div>
          )}
        </div>
      )}
    </div>
  );
});

/**
 * NoticesBanner — expandable banner showing policy warnings for a run
 */
const NoticesBanner = memo(({ notices }: { notices: RunNotice[] | undefined }) => {
  const [expanded, setExpanded] = useState(false);

  if (!notices || notices.length === 0) return null;

  return (
    <div
      className={styles.noticesBanner}
      onClick={() => setExpanded((prev) => !prev)}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => e.key === 'Enter' && setExpanded((prev) => !prev)}
    >
      <span>⚠</span>
      <span>
        {notices.length} policy warning{notices.length > 1 ? 's' : ''}
        {!expanded && ' — click to expand'}
      </span>
      {expanded && (
        <div className={styles.noticesList}>
          {notices.map((notice, i) => (
            <div key={i} className={styles.noticeItem}>
              {notice.message}
            </div>
          ))}
        </div>
      )}
    </div>
  );
});

export { NoticesBanner };
export default ChatMessageList;
