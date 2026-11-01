/**
 * ChatMessageList Component
 *
 * Shared message rendering used by both AssistantChat (workspace) and Fleet (detail pane).
 * Handles markdown rendering, tool call display, and auto-scrolling.
 */

import React, { useState, useCallback, useEffect, useMemo, memo } from 'react';
import MarkdownMessage from '../Chat/MarkdownMessage';
import StreamingMarkdown from '../Chat/StreamingMarkdown';
import { WorkspaceFileContext, useDocumentLocation } from '../Chat/WorkspaceFileContext';
import VirtualizedList from '../common/VirtualizedList';
import { isRunMarkerText } from '../../assistant/runMarker';
import type { RunFailure } from '../../assistant/runErrors';
import type { StreamedPart } from '../../assistant/sessionStore';
import RunFailureBanner from '../RunFailureBanner/RunFailureBanner';
import type {
  AssistantMessage,
  ContentPart,
  ToolInvocation,
  WorkspaceAgentResponse,
} from '../../generated/bindings';
import {
  asParamsObject,
  asPayloadObject,
  cleanToolName,
  extractMcpText,
  guessLang,
  inlineTaskCard,
  summarizeToolCall,
  toPreviewText,
  type TaskCallCard,
} from './toolDisplay';
import TaskCard, { TaskActionCard } from '../Agents/TaskCard';
import TaskResultCard from '../Agents/TaskResultCard';
import AgentAvatar from '../Agents/AgentAvatar';
import { identityRingColor, type AgentIdentity } from '../Agents/agentIdentity';
import { useAppTheme } from '../../hooks/useAppTheme';
import {
  resolveAgent,
  resolveMessageSender,
  senderGroupKey,
  type MessageSender,
  type SenderAgent,
  type SessionOwner,
} from './messageSender';
import { TaskCardContext, useTaskCardSurface, type TaskCardSurface } from './taskCardContext';
import { useTask } from '../../workspace/taskStore';
import styles from './AssistantChat.module.css';
import { onScrollChatToBottom } from '../../utils/workspaceUiEvents';
import ImageAttachment from './ImageAttachment';
import { imageParts, type ImagePart } from '../../assistant/imageParts';
import { useToolCallInput, useToolCallResult } from '../../assistant/toolCallResult';

// Tools beyond this count collapse behind a "show N earlier" toggle so a turn
// that fires dozens of tools stays scannable; the most-recent MAX_VISIBLE rows
// stay on screen.
const MAX_VISIBLE_TOOLS = 2;

// Narrowed ContentPart variants — `Extract` pulls the specific shape out
// of the generated discriminated union so `.text` / `.tool_name` etc.
// are accessible after a `type ===` guard.
type TextPart = Extract<ContentPart, { type: 'text' }>;
type ToolUsePart = Extract<ContentPart, { type: 'tool_use' }>;

// A tool_use enriched with the matching ToolInvocation record for rendering.
// `result` is the compact view: only charts and task cards keep one.
interface EnrichedToolUse {
  toolCallId: string;
  toolName: string;
  arguments?: unknown;
  status: string;
  params?: unknown;
  result?: unknown;
  error?: string | null;
  call?: ToolInvocation;
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

const EMPTY_STREAMING: Record<string, StreamedPart[]> = {};
const EMPTY_TOOL_CALLS: ToolInvocation[] = [];
const EMPTY_ROSTER: readonly WorkspaceAgentResponse[] = [];

const formatTimestamp = (timestamp: number | bigint | null | undefined): string => {
  if (!timestamp) return '';
  const date = new Date(Number(timestamp));
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
};

/**
 * Content parts arrive from the backend, so a malformed entry (a null slot,
 * a non-object) must not crash a reader that walks the array. Every content
 * reader below normalizes through this predicate, so one bad part costs
 * that part only — not the whole transcript.
 */
const isContentPart = (part: unknown): part is ContentPart =>
  !!part && typeof part === 'object';

/** The message's content parts, with malformed entries dropped. */
const contentParts = (message: AssistantMessage): ContentPart[] =>
  Array.isArray(message.content) ? message.content.filter(isContentPart) : [];

const getTextContent = (message: AssistantMessage): string =>
  contentParts(message)
    .filter((part): part is TextPart => part.type === 'text')
    .map((part) => part.text)
    .join('');

const getToolUses = (message: AssistantMessage): ToolUsePart[] =>
  contentParts(message).filter((part): part is ToolUsePart => part.type === 'tool_use');

const getImageContent = (message: AssistantMessage): ImagePart[] =>
  imageParts(contentParts(message));

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
    if (!isContentPart(part)) continue;
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
        call: tc,
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
 * Check if an assistant message has tool calls without any visible prose or thinking.
 */
const isToolOnlyMessage = (
  message: AssistantMessage,
  streamingParts: Record<string, StreamedPart[]>
): boolean => {
  if (message.role !== 'assistant' || streamingParts[message.id]?.length) return false;
  const hasTextOrThinking = contentParts(message).some((part) => {
    if (part.type !== 'text' && part.type !== 'thinking') return false;
    return typeof part.text === 'string' && part.text.trim().length > 0;
  });
  return !hasTextOrThinking && getToolUses(message).length > 0;
};

/**
 * Whether an assistant message has anything to show: a non-empty text or
 * thinking part, or any other part kind (tool_use etc.). A just-seeded turn
 * message (one empty Text placeholder) has none — rendering it produces a
 * zero-height item, and VirtualizedList can't cache a 0px measurement, so
 * the item holds its ~48px size estimate. That phantom slot is the gap that
 * flashes under the last tool row while the model composes its next tool
 * call. Callers hide such messages until content (or a live part) lands.
 */
const hasRenderableContent = (message: AssistantMessage): boolean =>
  contentParts(message).some((part) => {
    if (part.type === 'text' || part.type === 'thinking') return !!part.text;
    return true;
  });

const isHiddenMessage = (message: AssistantMessage | undefined): boolean => {
  if (!message) return true;
  if (message.role === 'tool') return true;

  if (message.role !== 'user') return false;
  const text = getTextContent(message);
  if (isRunMarkerText(text)) return true;
  // Hide only when there's nothing to show — neither text nor image parts. An
  // image-only user message (no caption) must still render its thumbnail.
  return !text && getImageContent(message).length === 0;
};

/**
 * Group consecutive tool-only assistant messages into merged blocks.
 * Returns an array of render items:
 * - { type: 'message', message } for normal messages
 * - { type: 'tool-group', messages: [...], toolUses: [...] } for merged tool-only turns
 */
const groupMessages = (
  messages: AssistantMessage[],
  streamingParts: Record<string, StreamedPart[]>
): RenderItem[] => {
  const result: RenderItem[] = [];
  let i = 0;

  while (i < messages.length) {
    const msg = messages[i]!; // bounded by the while condition

    if (isHiddenMessage(msg)) {
      i++;
      continue;
    }

    if (isToolOnlyMessage(msg, streamingParts)) {
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
        if (isToolOnlyMessage(candidate, streamingParts)) {
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

  // Unwrap MCP envelopes (objects with a content array, bare content arrays)
  // down to their text, then render that text by the rules below — an
  // envelope carrying JSON gets the same pretty-printed fence a bare JSON
  // string does, instead of one unreadable escaped line.
  const mcpText = typeof result === 'object' ? extractMcpText(result) : null;
  if (mcpText) {
    return renderToolResult(mcpText);
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

// Tools whose Output view draws from their input (the command, the question,
// the written content), so it needs the full input, not the row's projection.
const OUTPUT_READS_INPUT = new Set(['bash_exec', 'ask_user', 'fs_write']);

/** The loading or error state of a lazily fetched detail, or null once it is ready. */
const renderDetailPending = (
  detail: ReturnType<typeof useToolCallResult>,
  label: string
): React.ReactNode => {
  if (detail.state === 'loading') {
    return (
      <div className={styles.loadingState} role="status">
        <span>{`Loading ${label}…`}</span>
      </div>
    );
  }
  if (detail.state === 'error') {
    return (
      <div className={styles.toolResultError} role="alert">
        <span>{detail.message}</span>
        {detail.retryable && (
          <button type="button" className={styles.toolResultRetry} onClick={detail.retry}>
            Retry
          </button>
        )}
      </div>
    );
  }
  return null;
};

const formatElapsed = (ms: number): string => {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(totalSec / 60);
  const seconds = totalSec % 60;
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
};

interface RunningIndicatorProps {
  identity: AgentIdentity;
  runStartedAt?: number | null;
}

/**
 * RunningIndicator — the in-flight footer (left-aligned): the face of the
 * agent whose session this is, wearing its running expression, plus an
 * elapsed timer, so it's clear whose run is progressing and that it is.
 *
 * Nothing here animates, and nothing should: the face is drawn with
 * `ring={false}` so the status ring's spin never starts (the repaint budget
 * this protects is documented on `.runningIndicator` in the stylesheet). The
 * clock's 1s tick carries the "still alive" meaning for a fraction of the
 * cost, and carries it more honestly: a spinner keeps turning when the
 * backend is wedged, whereas a clock that stops has actually told you
 * something.
 *
 * Naming the in-flight tool here was tried and reverted: whenever it had
 * something to say, the transcript row directly above was already saying it,
 * so the footer read as a duplicated stray line detached from the block.
 */
const RunningIndicator = memo(({ identity, runStartedAt }: RunningIndicatorProps) => {
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
      <AgentAvatar identity={identity} size={22} activity="running" ring={false} label="Working" />
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
  // Workspace that owns the session: backs image thumbnails, workspace-relative
  // markdown links, and lazy tool-call detail loads.
  workspaceId: string;
  streamingParts?: Record<string, StreamedPart[]>;
  isStreaming?: boolean;
  toolCalls?: ToolInvocation[];
  // Failure of the most recent run, shown inline at the end of the
  // conversation (directly under the failed turn). Null when the last run
  // didn't fail.
  runFailure?: RunFailure | null;
  // Epoch ms when the in-flight run started, for the running indicator's
  // elapsed-time readout.
  runStartedAt?: number | null;
  // Whose face the in-flight footer wears. Omit to render no footer at all
  // while streaming: a task transcript is opened from a card that already
  // shows the assignee's running face, and its own header repeats it, so a
  // third marker inside the log is noise. Pass a reference that outlives the
  // render (a module constant, or a memo) — the footer memoizes on it.
  runningIdentity?: AgentIdentity | null;
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
  // The crew, so a delegated task's card wears the assignee's face and name.
  // Pass a reference that only changes when the crew itself does: it reaches
  // the cards by context, and a new array on every poll would repaint them.
  taskRoster?: readonly WorkspaceAgentResponse[];
  // Open a delegated task's own log, by task id. Omit in read-only views —
  // cards then render inert.
  onOpenTask?: (taskId: string) => void;
  // The agent `assistant` messages are signed with, resolved against
  // `taskRoster`. Omit for sessions no workspace agent owns ("Clai").
  owner?: SessionOwner;
}

const ChatMessageList = ({
  messages,
  workspaceId,
  streamingParts = EMPTY_STREAMING,
  isStreaming = false,
  toolCalls = EMPTY_TOOL_CALLS,
  runFailure = null,
  runStartedAt = null,
  runningIdentity = null,
  queuedMessageIds,
  onDeleteQueuedMessage,
  onEditQueuedMessage,
  hasOlderMessages = false,
  isLoadingOlderMessages = false,
  onLoadOlderMessages,
  taskRoster = EMPTY_ROSTER,
  onOpenTask,
  owner = null,
}: ChatMessageListProps) => {
  const ownerAgent = useMemo(
    () => (owner ? resolveAgent(owner, taskRoster) : null),
    [owner, taskRoster]
  );

  // Build a Map of toolCalls keyed by id once per render, so every
  // tool_use part lookup is O(1) instead of an Array.find walk. Memoized
  // on the toolCalls reference so the Map is stable while toolCalls
  // doesn't change, which keeps memoized children from re-rendering.
  const toolCallsById = useMemo(() => {
    const map = new Map<string, ToolInvocation>();
    for (const tc of toolCalls) map.set(tc.id, tc);
    return map;
  }, [toolCalls]);

  // Lets markdown in messages resolve workspace-relative references (a
  // `.vl.json` chart link, a spec's `data.url`, an image, a file link)
  // against the workspace root.
  const fileLocation = useDocumentLocation(workspaceId, '');

  // External scroll-to-bottom nudges (e.g. entering terminal mode shrinks the
  // conversation viewport). Folded into scrollToBottomSignal below with a wide
  // multiplier so it never collides with the messages.length component. The
  // underlying VirtualizedList still honours "only if the reader was near the
  // bottom", so a reader scrolled up into history is left undisturbed.
  const [scrollNudge, setScrollNudge] = useState(0);
  useEffect(() => onScrollChatToBottom(() => setScrollNudge((n) => n + 1)), []);

  // Drop assistant messages that have nothing to render yet — the empty
  // placeholder each turn stays hidden until content or a live part arrives.
  const visibleMessages = useMemo(
    () =>
      messages.filter(
        (msg) => msg.role !== 'assistant' || hasRenderableContent(msg) || !!streamingParts[msg.id]?.length
      ),
    [messages, streamingParts]
  );

  const grouped = useMemo(() => {
    const items = groupMessages(visibleMessages, streamingParts);
    if (hasOlderMessages && onLoadOlderMessages) {
      return [{ type: 'load-earlier' } as RenderItem, ...items];
    }
    return items;
  }, [visibleMessages, streamingParts, hasOlderMessages, onLoadOlderMessages]);

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

  const senders = useMemo(
    () =>
      new Map(
        visibleMessages.map((m) => [m.id, resolveMessageSender(m, ownerAgent, taskRoster)])
      ),
    [visibleMessages, ownerAgent, taskRoster]
  );

  // An item is a continuation when the item before it has the same sender
  // group (see senderGroupKey): it renders slim, without a header, flowing
  // into the previous block. Tool groups are the owner's.
  const continuationFlags = useMemo(() => {
    const groupKey = (it: RenderItem | undefined) => {
      if (!it || it.type === 'load-earlier') return null;
      if (it.type === 'tool-group') return 'owner';
      const sender = senders.get(it.message.id);
      return sender ? senderGroupKey(sender) : null;
    };

    return grouped.map((it, idx) => {
      const key = groupKey(it);
      return key !== null && key === groupKey(grouped[idx - 1]);
    });
  }, [grouped, senders]);

  const itemKey = useCallback(
    (item: RenderItem) =>
      item.type === 'load-earlier'
        ? 'load-earlier'
        : item.type === 'tool-group'
          ? `tool-group:${item.id}`
          : `message:${item.message.id}`,
    []
  );

  const throttledMeasureKeys = useMemo(() => {
    if (!isStreaming) return undefined;
    const streamingMessageIds = Object.keys(streamingParts).filter((id) => streamingParts[id]?.length);
    if (streamingMessageIds.length === 0) return undefined;
    return new Set(streamingMessageIds.map((id) => `message:${id}`));
  }, [isStreaming, streamingParts]);

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
          workspaceId={workspaceId}
          toolCallsById={toolCallsById}
          isContinuation={continuationFlags[idx]}
          owner={ownerAgent}
        />
      ) : (
        <MessageBlock
          message={item.message}
          streamingParts={streamingParts[item.message.id]}
          toolCallsById={toolCallsById}
          sender={senders.get(item.message.id)}
          workspaceId={workspaceId}
          isContinuation={continuationFlags[idx]}
          isQueued={queuedIdSet.has(item.message.id)}
          onDeleteQueued={onDeleteQueuedMessage}
          onEditQueued={onEditQueuedMessage}
          owner={ownerAgent}
        />
      ),
    [
      continuationFlags,
      ownerAgent,
      streamingParts,
      toolCallsById,
      senders,
      workspaceId,
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

  const taskCardSurface = useMemo<TaskCardSurface>(
    () => ({ workspaceId, roster: taskRoster, onOpenTask }),
    [workspaceId, taskRoster, onOpenTask]
  );

  // Footer rendered inside the scroll area, right after the last message.
  // While a run is in flight we show the activity indicator; once it ends we
  // show the failure (if any) attached to the turn it belongs to. These are
  // mutually exclusive — a failed run is no longer streaming.
  const footer = isStreaming ? (
    runningIdentity ? (
      <RunningIndicator identity={runningIdentity} runStartedAt={runStartedAt} />
    ) : null
  ) : runFailure ? (
    <RunFailureBanner failure={runFailure} className={styles.runFailure} />
  ) : null;

  return (
    <WorkspaceFileContext.Provider value={fileLocation}>
      <TaskCardContext.Provider value={taskCardSurface}>
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
          throttledMeasureKeys={throttledMeasureKeys}
          onApproachTop={handleApproachTop}
        />
      </TaskCardContext.Provider>
    </WorkspaceFileContext.Provider>
  );
};

interface MessageBlockProps {
  message: AssistantMessage;
  streamingParts?: StreamedPart[];
  toolCallsById: Map<string, ToolInvocation>;
  sender?: MessageSender;
  workspaceId: string;
  isContinuation?: boolean;
  isQueued?: boolean;
  onDeleteQueued?: (messageId: string) => void;
  onEditQueued?: (messageId: string, text: string) => Promise<void> | void;
  owner: SenderAgent | null;
}

const MessageBlock = memo(({ message, ...props }: MessageBlockProps) => {
  if (message.role === 'assistant') return <AssistantMessageBlock message={message} {...props} />;
  // Skip tool result messages — shown inline with tool calls
  if (message.role !== 'user') return null;
  const sender = props.sender ?? { kind: 'user' };
  if (sender.kind === 'user') return <UserMessageBlock message={message} {...props} />;
  if (sender.kind === 'task') return <TaskResultBlock sender={sender} />;
  return (
    <InboxMessageBlock
      message={message}
      agent={sender.kind === 'agent' ? sender.agent : null}
      isContinuation={props.isContinuation}
    />
  );
});

const UserMessageBlock = ({
  message,
  workspaceId,
  isQueued = false,
  onDeleteQueued,
  onEditQueued,
}: MessageBlockProps) => {
  const { createdAt } = message;
  // Local draft state for editing a queued message in-place. `null` = not
  // editing; a string = the working draft seeded from the message text.
  const [editDraft, setEditDraft] = useState<string | null>(null);
  const textContent = getTextContent(message);
  const imageParts = getImageContent(message);
  if (!textContent && imageParts.length === 0) return null;

  if (isRunMarkerText(textContent)) return null;

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
        <span className={styles.messageRoleText}>You</span>
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
          {textContent && <MarkdownMessage content={textContent} />}
          {imageParts.length > 0 && (
            <div className={styles.imageAttachments}>
              {imageParts.map((part) => (
                <ImageAttachment key={part.id} workspaceId={workspaceId} part={part} />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
};

const AssistantMessageBlock = ({
  message,
  streamingParts,
  toolCallsById,
  workspaceId,
  isContinuation = false,
  owner,
}: MessageBlockProps) => {
  // Walk message.content in order, grouping consecutive same-type
  // parts into segments. This preserves the interleaving (text → tool
  // → text → tool …) the assistant actually produced, rather than
  // collapsing all text to the top and all tools to the bottom.
  // Live parts follow persisted content and keep their text/thinking order.
  // Memoized so
  // ToolCallGroup's prop reference is stable across re-renders when
  // nothing actually changed — its memo only helps if its toolUses
  // array reference doesn't churn.
  const segments = useMemo(() => {
    const base = groupAssistantContent(message.content, toolCallsById);
    for (const part of streamingParts ?? []) {
      // A live part continues the block the persisted content ends with
      // when both are the same kind. The persisted view merges consecutive
      // same-kind parts into one block (groupAssistantContent), so merging
      // here keeps the live view identical to what the message renders once
      // the part is persisted. Unmerged, the same content shows as two
      // "Thinking" blocks and then collapses into one when it is persisted.
      const last = base[base.length - 1];
      if (part.type === 'text' && last?.kind === 'text') {
        last.text += part.text;
        last.streaming = true;
        continue;
      }
      if (part.type === 'thinking' && last?.kind === 'thinking') {
        last.text += part.text;
        continue;
      }
      base.push({
        kind: part.type,
        text: part.text,
        ...(part.type === 'text' && { streaming: true }),
      });
    }
    return base;
  }, [message.content, toolCallsById, streamingParts]);

  return (
    <div className={isContinuation ? styles.assistantContinuation : styles.assistantMessage}>
      {!isContinuation && <OwnerHeader owner={owner} createdAt={message.createdAt} />}
      <div className={styles.messageContent}>
        {segments.map((seg, idx) => {
          if (seg.kind === 'thinking') {
            return <ThinkingBlock key={idx} content={seg.text} />;
          }
          if (seg.kind === 'text') {
            return <StreamingMarkdown key={idx} content={seg.text} isStreaming={!!seg.streaming} />;
          }
          if (seg.kind === 'tools') {
            return <ToolCallGroup key={idx} workspaceId={workspaceId} toolUses={seg.toolUses} owner={owner} />;
          }
          return null;
        })}
      </div>
    </div>
  );
};

const TaskResultBlock = ({ sender }: { sender: Extract<MessageSender, { kind: 'task' }> }) => {
  const { workspaceId = '', onOpenTask } = useTaskCardSurface();
  const liveTask = useTask(workspaceId, sender.taskId);
  const sameCycle = liveTask && Number(liveTask.cycleNo) === sender.cycleNo;
  return (
    <TaskResultCard
      sender={sender}
      failureReason={sameCycle ? liveTask.failureReason : null}
      onOpen={onOpenTask}
    />
  );
};

/**
 * A message another agent sent into this session, or one from an origin this
 * build does not know ("System"). Never the user's style, never editable.
 */
const InboxMessageBlock = ({
  message,
  agent,
  isContinuation = false,
}: {
  message: AssistantMessage;
  agent: SenderAgent | null;
  isContinuation?: boolean;
}) => {
  const theme = useAppTheme();
  const style = agent
    ? ({ '--sender-hue': identityRingColor(agent.identity, theme) } as React.CSSProperties)
    : undefined;
  return (
    <div className={styles.inboxMessage} style={style}>
      {isContinuation ? (
        <span className="sr-only">{agent ? agent.name : 'System'}:</span>
      ) : (
        <SenderHeader agent={agent} fallbackName="System" createdAt={message.createdAt} />
      )}
      <div className={styles.messageContent}>
        <MarkdownMessage content={getTextContent(message)} />
      </div>
    </div>
  );
};

/** The owner's face and name over an assistant turn; "Clai" when no agent owns the session. */
const OwnerHeader = ({
  owner,
  createdAt,
}: {
  owner: SenderAgent | null;
  createdAt: number | bigint | null | undefined;
}) => <SenderHeader agent={owner} fallbackName="Clai" createdAt={createdAt} />;

const SenderHeader = ({
  agent,
  fallbackName,
  createdAt,
}: {
  agent: SenderAgent | null;
  fallbackName: string;
  createdAt: number | bigint | null | undefined;
}) => (
  <div className={styles.messageHeader}>
    {agent && (
      <AgentAvatar
        identity={agent.identity}
        size={20}
        activity={agent.presence === 'disabled' ? 'disabled' : 'none'}
      />
    )}
    <span className={styles.messageRoleText}>{agent ? agent.name : fallbackName}</span>
    {agent && agent.presence !== 'active' && (
      <span className={styles.senderSuffix}>({agent.presence})</span>
    )}
    {createdAt && <span className={styles.messageTimestamp}>{formatTimestamp(createdAt)}</span>}
  </div>
);

/**
 * MergedToolGroup — renders tool calls from multiple consecutive assistant turns
 * as a single collapsed group, avoiding repeated "CLAI" headers for tool-only turns.
 */
interface MergedToolGroupProps {
  item: Extract<RenderItem, { type: 'tool-group' }>;
  workspaceId: string;
  toolCallsById: Map<string, ToolInvocation>;
  isContinuation?: boolean;
  owner: SenderAgent | null;
}

const MergedToolGroup = memo(
  ({ item, workspaceId, toolCallsById, isContinuation = false, owner }: MergedToolGroupProps) => {
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
            call: tc,
          };
        }),
      [item.toolUses, toolCallsById]
    );

    return (
      <div className={isContinuation ? styles.assistantContinuation : styles.assistantMessage}>
        {!isContinuation && <OwnerHeader owner={owner} createdAt={item.createdAt} />}
        <div className={styles.messageContent}>
          <ToolCallGroup workspaceId={workspaceId} toolUses={enrichedToolUses} owner={owner} />
        </div>
      </div>
    );
  }
);

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
        <span>Executing…</span>
      </div>
    );
  }

  if (name === 'bash_exec') {
    const command = asParamsObject(params)?.command;
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
    const p = asParamsObject(params);
    const r = asPayloadObject(result);
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
    const fromResult = asPayloadObject(result);
    const fromParams = asParamsObject(params);
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

/** A tool call with the task card it draws, when it draws one. */
interface ToolItem {
  toolUse: EnrichedToolUse;
  card: TaskCallCard | null;
}

/**
 * A tool list split at the rows that render something the user came for: a
 * task hand-off, a task's answer. Those are the agent's output, not
 * noise, so they are never collapsed; the plain rows on either side collapse
 * as independent runs.
 *
 * A poll that found its task still running is not an answer — it stays in the
 * run as a slim line, so an agent waiting through a dozen polls costs a dozen
 * quiet lines that collapse together.
 */
type ToolSegment =
  // `key` is the first call's id so a run keeps its expanded/collapsed
  // state as later calls append to it.
  | { kind: 'rows'; key: string; items: ToolItem[] }
  | { kind: 'task'; key: string; card: TaskCallCard; call?: ToolInvocation };

const splitAtRichRows = (toolUses: EnrichedToolUse[]): ToolSegment[] => {
  const segments: ToolSegment[] = [];
  let run: ToolItem[] = [];
  const flushRun = () => {
    const first = run[0];
    if (first) segments.push({ kind: 'rows', key: first.toolUse.toolCallId, items: run });
    run = [];
  };
  for (const tu of toolUses) {
    const card = inlineTaskCard(tu.toolName, tu.result, tu.error, tu.status);
    if (card && card.variant === 'full') {
      flushRun();
      segments.push({ kind: 'task', key: tu.toolCallId, card, call: tu.call });
      continue;
    }
    run.push({ toolUse: tu, card });
  }
  flushRun();
  return segments;
};

/**
 * A task card wired to the page around the chat. Reading the crew and the
 * open-task handler from context here — rather than threading them as props —
 * keeps every memoized message and tool group untouched when they change.
 */
const ChatTaskCard = memo(
  ({ card, call, origin = null }: { card: TaskCallCard; call?: ToolInvocation; origin?: SenderAgent | null }) => {
    const { workspaceId = '', roster, onOpenTask } = useTaskCardSurface();
    if (card.kind === 'assign' || card.kind === 'poll') {
      return <TaskCard card={card} roster={roster} onOpen={onOpenTask} />;
    }
    const preview = card.kind === 'message' ? asParamsObject(call?.params)?.message : undefined;
    return (
      <TaskActionCard card={card} origin={origin} roster={roster} onOpen={onOpenTask}>
        {call && typeof preview === 'string' && preview && (
          <SentTaskMessage workspaceId={workspaceId} call={call} preview={preview} />
        )}
      </TaskActionCard>
    );
  }
);

/**
 * A task message's text. The chat's params carry it clipped to 300 characters
 * and marked with a trailing `…` (see `compact_input` in display.rs); only a
 * clipped one fetches the rest, on demand.
 */
const SentTaskMessage = ({
  workspaceId,
  call,
  preview,
}: {
  workspaceId: string;
  call: ToolInvocation;
  preview: string;
}) => {
  const [open, setOpen] = useState(false);
  const clipped = preview.endsWith('…');
  const input = useToolCallInput(workspaceId, call, open && clipped);
  const fromInput = input.state === 'ready' ? asParamsObject(input.value)?.message : undefined;
  const full = open && typeof fromInput === 'string' ? fromInput : preview;
  return (
    <>
      {(open && renderDetailPending(input, 'message')) || <MarkdownMessage content={full} />}
      {clipped && (
        <button
          type="button"
          className={styles.toolShowEarlier}
          onClick={() => setOpen((prev) => !prev)}
          aria-expanded={open}
        >
          {open ? 'Show less' : 'Show full message'}
        </button>
      )}
    </>
  );
};

interface ToolCallGroupProps {
  workspaceId: string;
  toolUses: EnrichedToolUse[];
  /** Whose turn made these calls: the sender of a task message or stop. */
  owner: SenderAgent | null;
}

/**
 * ToolCallGroup — renders a turn's tool calls as compact one-line rows.
 * Beyond MAX_VISIBLE_TOOLS, older calls collapse behind a "show N earlier"
 * toggle so a 35-tool turn stays scannable.
 *
 * A full task card breaks the count: it stays on screen wherever it was
 * produced and the calls that follow it collapse *below* it.
 */
const ToolCallGroup = memo(({ workspaceId, toolUses, owner }: ToolCallGroupProps) => {
  const segments = useMemo(() => splitAtRichRows(toolUses), [toolUses]);

  if (toolUses.length === 0) return null;

  return (
    <div className={styles.toolList}>
      {segments.map((seg) =>
        seg.kind === 'task' ? (
          <ChatTaskCard key={seg.key} card={seg.card} call={seg.call} origin={owner} />
        ) : (
          <CollapsibleToolRows key={seg.key} workspaceId={workspaceId} items={seg.items} />
        )
      )}
    </div>
  );
});

/**
 * CollapsibleToolRows — one run of plain tool rows. Beyond MAX_VISIBLE_TOOLS
 * the oldest collapse behind a "show N earlier" toggle; the most-recent rows
 * stay on screen.
 */
const CollapsibleToolRows = memo(({ workspaceId, items }: { workspaceId: string; items: ToolItem[] }) => {
  const [showEarlier, setShowEarlier] = useState(false);

  const overflow = items.length - MAX_VISIBLE_TOOLS;
  const hasOverflow = overflow > 0;
  const visible = hasOverflow && !showEarlier ? items.slice(-MAX_VISIBLE_TOOLS) : items;

  return (
    <>
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
      {visible.map(({ toolUse: tu, card }) =>
        card ? (
          <ChatTaskCard key={tu.toolCallId} card={card} />
        ) : (
          <ToolRow
            key={tu.toolCallId}
            workspaceId={workspaceId}
            toolName={tu.toolName}
            params={tu.params ?? tu.arguments}
            status={tu.status}
            error={tu.error}
            call={tu.call}
          />
        )
      )}
    </>
  );
});

/**
 * ToolRow — a single tool call as one scannable line:
 *   <status> <verb> <primary arg> ............... <result summary> <chevron>
 * Click to expand the full, well-formatted Output/Input view.
 */
interface ToolRowProps {
  workspaceId: string;
  toolName: string;
  params?: unknown;
  status: string;
  error?: string | null;
  call?: ToolInvocation;
}

const ToolRow = memo(({ workspaceId, toolName, params, status, error, call }: ToolRowProps) => {
  const [isExpanded, setIsExpanded] = useState(false);
  const [activeTab, setActiveTab] = useState<'output' | 'input'>('output');

  const handleToggle = useCallback(() => setIsExpanded((prev) => !prev), []);

  const { verb, arg } = useMemo(() => summarizeToolCall(toolName, params), [toolName, params]);
  const isRunning = status === 'running';
  const isFailed = status === 'failed' || !!error;
  const icon = isFailed ? '✗' : isRunning ? '⚙' : '✓';
  const resultSummary =
    call?.resultSummary ?? (isFailed && !isRunning ? { text: 'error', tone: 'error' } : null);

  // formatParams hides empty params — right for tools legitimately called
  // with no args, but on a failed call "the model sent {}" is exactly what
  // the user needs to see (e.g. a schema-validation reject for a missing
  // required property), so fall back to the raw JSON there.
  const formatInput = (input: unknown) =>
    formatParams(input) ?? (isFailed && input != null ? JSON.stringify(input, null, 2) : null);
  // `params` is a projection of the input; the full one is fetched when a
  // view needs more than the row shows.
  const hasFullInput = !!call?.hasFullInput;
  const hasInput = hasFullInput || !!formatInput(params);
  const hasFullResult = !!call?.hasFullResult;
  const hasOutput = hasFullResult || !!error || isRunning;
  const outputReadsInput = OUTPUT_READS_INPUT.has(cleanToolName(toolName || ''));
  const fullResult = useToolCallResult(
    workspaceId,
    call,
    isExpanded && activeTab === 'output' && hasFullResult && !isRunning
  );
  const fullInput = useToolCallInput(
    workspaceId,
    call,
    isExpanded &&
      hasFullInput &&
      (activeTab === 'input' || (activeTab === 'output' && outputReadsInput && !isRunning))
  );
  const input = fullInput.state === 'ready' ? fullInput.value : params;
  // The output view degrades to the projected input if the full one fails.
  const outputPending =
    renderDetailPending(fullResult, 'output') ??
    (outputReadsInput && fullInput.state === 'loading'
      ? renderDetailPending(fullInput, 'output')
      : null);

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
            <span className={styles.toolRowRunning}>running…</span>
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
              {renderDetailPending(fullInput, 'input') ?? (
                <MarkdownMessage
                  content={'```json\n' + (formatInput(input) ?? '{}') + '\n```'}
                  isStreaming={false}
                />
              )}
            </div>
          )}

          {activeTab === 'output' && (
            <div className={styles.toolResult}>
              {outputPending ??
                renderToolOutput(
                  toolName,
                  input,
                  fullResult.state === 'ready' ? fullResult.value : null,
                  error,
                  isRunning
                )}
            </div>
          )}
        </div>
      )}
    </div>
  );
});

export default ChatMessageList;
