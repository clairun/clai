/**
 * ChatMessageList Component
 *
 * Shared message rendering used by both AssistantChat (workspace) and Fleet (detail pane).
 * Handles markdown rendering, tool call display, and auto-scrolling.
 */

import React, { useEffect, useRef, useState, useCallback, memo } from 'react';
import MarkdownMessage from '../Chat/MarkdownMessage';
import StreamingMarkdown from '../Chat/StreamingMarkdown';
import styles from './AssistantChat.module.css';

const EMPTY_STREAMING = {};
const EMPTY_TOOL_CALLS = [];

const formatTimestamp = (timestamp) => {
  if (!timestamp) return '';
  const date = new Date(timestamp);
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
};

const getTextContent = (message) => {
  if (!message.content || !Array.isArray(message.content)) return '';
  return message.content
    .filter((part) => part.type === 'text')
    .map((part) => part.text)
    .join('');
};

const getToolUses = (message) => {
  if (!message.content || !Array.isArray(message.content)) return [];
  return message.content.filter((part) => part.type === 'tool_use');
};

const getThinkingContent = (message) => {
  if (!message.content || !Array.isArray(message.content)) return '';
  return message.content
    .filter((part) => part.type === 'thinking')
    .map((part) => part.text)
    .join('');
};

/**
 * Collapsible "thinking" block — renders the model's reasoning_content
 * with a distinct muted/italic style so it doesn't compete with the
 * user-facing response. Collapsed by default; click to expand.
 */
const ThinkingBlock = memo(({ content }) => {
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
        <span className={styles.thinkingIcon} aria-hidden="true">{'\u{1F4AD}'}</span>
        <span className={styles.thinkingLabel}>Thinking</span>
        {!expanded && preview && (
          <span className={styles.thinkingPreview}>{preview}{content.length > preview.length ? '…' : ''}</span>
        )}
        <span className={styles.thinkingChevron} aria-hidden="true">{expanded ? '▾' : '▸'}</span>
      </button>
      {expanded && <pre className={styles.thinkingBody}>{content}</pre>}
    </div>
  );
});
ThinkingBlock.displayName = 'ThinkingBlock';

/**
 * Clean MCP-style tool names: "mcp.<uuid>.get_metric_data" → "get_metric_data"
 */
const cleanToolName = (name) => {
  if (!name) return name;
  // Match mcp.<uuid-or-id>.<actual_tool_name>
  const match = name.match(/^mcp\.[^.]+\.(.+)$/);
  return match ? match[1] : name;
};

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
const groupAssistantContent = (content, toolCalls) => {
  if (!Array.isArray(content)) return [];
  const segments = [];
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
      const tc = toolCalls.find((t) => t.id === part.tool_call_id);
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
const isToolOnlyMessage = (message) => {
  if (message.role !== 'assistant') return false;
  const text = getTextContent(message);
  const tools = getToolUses(message);
  return !text.trim() && tools.length > 0;
};

/**
 * Group consecutive tool-only assistant messages into merged blocks.
 * Returns an array of render items:
 * - { type: 'message', message } for normal messages
 * - { type: 'tool-group', messages: [...], toolUses: [...] } for merged tool-only turns
 */
const groupMessages = (messages) => {
  const result = [];
  let i = 0;

  while (i < messages.length) {
    const msg = messages[i];

    if (isToolOnlyMessage(msg)) {
      // Collect consecutive tool-only assistant messages
      const group = [msg];
      let j = i + 1;
      while (j < messages.length) {
        // Skip tool-result messages (role: tool) between assistant turns
        if (messages[j].role === 'tool') {
          j++;
          continue;
        }
        if (isToolOnlyMessage(messages[j])) {
          group.push(messages[j]);
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
          createdAt: group[0].createdAt,
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
/**
 * Extract displayable text from an MCP-style result.
 * MCP results can be:
 * - An envelope object: { content: [{ type: "text", text: "..." }], text: "...", ... }
 * - A content array directly: [{ type: "text", text: "..." }]
 * - A plain string
 * - A generic JSON object
 */
const extractMcpText = (result) => {
  if (!result || typeof result !== 'object') return null;

  // Envelope object with content array
  if (result.content && Array.isArray(result.content)) {
    const textParts = result.content
      .filter((p) => p && p.type === 'text' && typeof p.text === 'string')
      .map((p) => p.text);
    if (textParts.length > 0) return textParts.join('\n\n');
  }

  // Envelope object with top-level text field
  if (typeof result.text === 'string' && result.text.trim()) {
    return result.text;
  }

  // Direct content array
  if (Array.isArray(result)) {
    const textParts = result
      .filter((p) => p && p.type === 'text' && typeof p.text === 'string')
      .map((p) => p.text);
    if (textParts.length > 0) return textParts.join('\n\n');
  }

  return null;
};

const renderToolResult = (result) => {
  if (result == null) return null;

  // Try MCP text extraction first (handles envelope objects and content arrays)
  const mcpText = typeof result === 'object' ? extractMcpText(result) : null;
  if (mcpText) {
    return <MarkdownMessage content={mcpText} isStreaming={false} />;
  }

  // String: render as markdown (detect JSON strings)
  if (typeof result === 'string') {
    const trimmed = result.trim();
    if ((trimmed.startsWith('{') && trimmed.endsWith('}')) || (trimmed.startsWith('[') && trimmed.endsWith(']'))) {
      try {
        const parsed = JSON.parse(trimmed);
        return <MarkdownMessage content={'```json\n' + JSON.stringify(parsed, null, 2) + '\n```'} isStreaming={false} />;
      } catch { /* not JSON, render as markdown */ }
    }
    return <MarkdownMessage content={result} isStreaming={false} />;
  }

  // Object/array without MCP text: pretty-print as JSON
  if (typeof result === 'object') {
    return <MarkdownMessage content={'```json\n' + JSON.stringify(result, null, 2) + '\n```'} isStreaming={false} />;
  }

  return <span>{String(result)}</span>;
};

/**
 * Format tool parameters for display.
 * Returns null if params are empty/null.
 */
const formatParams = (params) => {
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

/**
 * ChatMessageList - Renders a list of assistant messages with markdown and tool calls
 */
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
}) => {
  const messagesEndRef = useRef(null);
  const containerRef = useRef(null);
  const isNearBottomRef = useRef(true);
  const prevMessageCountRef = useRef(0);

  const checkIfNearBottom = useCallback(() => {
    const container = containerRef.current;
    if (!container) return true;
    const threshold = 150;
    return container.scrollHeight - container.scrollTop - container.clientHeight < threshold;
  }, []);

  const handleScroll = useCallback(() => {
    isNearBottomRef.current = checkIfNearBottom();
  }, [checkIfNearBottom]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const currentCount = messages.length;
    const isNewMessage = currentCount > prevMessageCountRef.current;
    prevMessageCountRef.current = currentCount;

    // New message boundary: do a single smooth scroll to anchor the
    // conversation at the new turn. After that, while content is still
    // flowing in, we pin the viewport to the bottom directly (no
    // animation) so the typewriter / tool blocks grow in place without
    // fighting an in-flight smooth-scroll animation.
    if (isNewMessage) {
      messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
      isNearBottomRef.current = true;
      return;
    }

    if (isStreaming && isNearBottomRef.current) {
      container.scrollTop = container.scrollHeight - container.clientHeight;
    }
  }, [messages, isStreaming, streamingText, toolCalls]);

  const grouped = groupMessages(messages);

  // An item is an "assistant continuation" when it's an assistant turn
  // (text+tools MessageBlock or a tool-only MergedToolGroup) AND the
  // item before it is also assistant/tool. Continuations render slim:
  // no header, no card chrome, just the content flowing into the
  // previous block. The first assistant after a user message keeps
  // the full header and card framing so the turn boundary stays
  // legible.
  const isAssistantItem = (it) => {
    if (!it) return false;
    if (it.type === 'tool-group') return true;
    return it.message?.role === 'assistant';
  };
  const continuationFlags = grouped.map((_, idx) =>
    isAssistantItem(grouped[idx]) && isAssistantItem(grouped[idx - 1])
  );

  return (
    <div
      ref={containerRef}
      className={styles.activityList}
      onScroll={handleScroll}
    >
      {grouped.map((item, idx) =>
        item.type === 'tool-group' ? (
          <MergedToolGroup
            key={item.id}
            item={item}
            toolCalls={toolCalls}
            isContinuation={continuationFlags[idx]}
          />
        ) : (
          <MessageBlock
            key={item.message.id}
            message={item.message}
            streamingText={streamingText[item.message.id]}
            toolCalls={toolCalls}
            userLabel={userLabel}
            isContinuation={continuationFlags[idx]}
          />
        )
      )}

      {isStreaming && (
        <div className={styles.runningIndicator}>
          <img
            src="/icon.svg"
            alt="Clai"
            className={styles.runningIcon}
          />
        </div>
      )}

      <div ref={messagesEndRef} />
    </div>
  );
};

const MessageBlock = memo(({ message, streamingText, toolCalls, userLabel = 'You', isContinuation = false }) => {
  const { role, createdAt } = message;

  if (role === 'user') {
    const textContent = getTextContent(message);
    if (!textContent) return null;

    // Hide run boundary markers (persisted trigger messages for the LLM, not for the user)
    if (textContent.startsWith('--- New scheduled run at') || textContent.startsWith('--- Manual run at')) {
      return null;
    }

    return (
      <div className={styles.userMessage}>
        <div className={styles.messageHeader}>
          <span className={styles.messageRoleText}>{userLabel}</span>
          {createdAt && <span className={styles.messageTimestamp}>{formatTimestamp(createdAt)}</span>}
        </div>
        <div className={styles.messageContent}>
          <MarkdownMessage content={textContent} />
        </div>
      </div>
    );
  }

  if (role === 'assistant') {
    const isCurrentlyStreaming = !!streamingText;
    // Walk message.content in order, grouping consecutive same-type
    // parts into segments. This preserves the interleaving (text → tool
    // → text → tool …) the assistant actually produced, rather than
    // collapsing all text to the top and all tools to the bottom.
    // streamingText is appended as a trailing segment because by
    // definition it represents the *current* in-flight text block,
    // which sits after everything already persisted.
    const segments = groupAssistantContent(message.content, toolCalls);
    if (isCurrentlyStreaming) {
      segments.push({ kind: 'text', text: streamingText, streaming: true });
    }

    return (
      <div className={isContinuation ? styles.assistantContinuation : styles.assistantMessage}>
        {!isContinuation && (
          <div className={styles.messageHeader}>
            <span className={styles.messageRoleText}>Clai</span>
            {createdAt && <span className={styles.messageTimestamp}>{formatTimestamp(createdAt)}</span>}
          </div>
        )}
        <div className={styles.messageContent}>
          {segments.map((seg, idx) => {
            if (seg.kind === 'thinking') {
              return <ThinkingBlock key={idx} content={seg.text} />;
            }
            if (seg.kind === 'text') {
              return (
                <StreamingMarkdown
                  key={idx}
                  content={seg.text}
                  isStreaming={!!seg.streaming}
                />
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
});

/**
 * MergedToolGroup — renders tool calls from multiple consecutive assistant turns
 * as a single collapsed group, avoiding repeated "CLAI" headers for tool-only turns.
 */
const MergedToolGroup = memo(({ item, toolCalls, isContinuation = false }) => {
  const enrichedToolUses = item.toolUses.map((tu) => {
    const tc = toolCalls.find((t) => t.id === tu.tool_call_id);
    return {
      toolCallId: tu.tool_call_id,
      toolName: cleanToolName(tu.tool_name),
      arguments: tu.arguments,
      status: tc?.status || 'running',
      params: tc?.params,
      result: tc?.result,
      error: tc?.error,
    };
  });

  return (
    <div className={isContinuation ? styles.assistantContinuation : styles.assistantMessage}>
      {!isContinuation && (
        <div className={styles.messageHeader}>
          <span className={styles.messageRoleText}>Clai</span>
          {item.createdAt && <span className={styles.messageTimestamp}>{formatTimestamp(item.createdAt)}</span>}
        </div>
      )}
      <div className={styles.messageContent}>
        <ToolCallGroup toolUses={enrichedToolUses} />
      </div>
    </div>
  );
});

/**
 * ToolCallGroup — collapses multiple tool calls into a compact summary.
 * Single tool call: shown inline (always visible header).
 * Multiple tool calls: collapsed behind a summary row.
 */
const ToolCallGroup = memo(({ toolUses }) => {
  const [isGroupExpanded, setIsGroupExpanded] = useState(false);

  if (toolUses.length === 0) return null;

  // Single tool call — render inline, no grouping needed
  if (toolUses.length === 1) {
    const tu = toolUses[0];
    return (
      <ToolCallBlock
        key={tu.toolCallId}
        toolName={tu.toolName}
        params={tu.params || tu.arguments}
        status={tu.status}
        result={tu.result}
        error={tu.error}
      />
    );
  }

  // Multiple tool calls — group with summary
  const completedCount = toolUses.filter((t) => t.status === 'completed').length;
  const failedCount = toolUses.filter((t) => t.status === 'failed').length;
  const runningCount = toolUses.length - completedCount - failedCount;

  const summaryParts = [];
  if (completedCount > 0) summaryParts.push(`${completedCount} complete`);
  if (failedCount > 0) summaryParts.push(`${failedCount} failed`);
  if (runningCount > 0) summaryParts.push(`${runningCount} running`);

  return (
    <div className={styles.toolGroup}>
      <div
        className={styles.toolGroupHeader}
        onClick={() => setIsGroupExpanded((prev) => !prev)}
      >
        <div className={styles.toolHeaderLeft}>
          <span className={styles.toolIconEmoji}>
            {failedCount > 0 ? '✗' : runningCount > 0 ? '⚙' : '✓'}
          </span>
          <span className={styles.toolName}>
            {toolUses.length} tool calls
          </span>
          <span className={styles.toolGroupSummary}>
            {summaryParts.join(' · ')}
          </span>
        </div>
        <div className={styles.toolHeaderRight}>
          <span className={`${styles.expandIcon} ${isGroupExpanded ? styles.expanded : ''}`}>
            ▼
          </span>
        </div>
      </div>

      {isGroupExpanded && (
        <div className={styles.toolGroupBody}>
          {toolUses.map((tu) => (
            <ToolCallBlock
              key={tu.toolCallId}
              toolName={tu.toolName}
              params={tu.params || tu.arguments}
              status={tu.status}
              result={tu.result}
              error={tu.error}
            />
          ))}
        </div>
      )}
    </div>
  );
});

/**
 * ToolCallBlock — renders a single tool call with status, params, and result
 */
const ToolCallBlock = memo(({ toolName, params, status, result, error }) => {
  const [isExpanded, setIsExpanded] = useState(false);
  const [activeTab, setActiveTab] = useState('output');

  const handleToggle = useCallback(() => {
    setIsExpanded((prev) => !prev);
  }, []);

  const statusDisplay = status === 'completed' ? 'complete' : status === 'failed' ? 'error' : 'pending';
  const formattedParams = formatParams(params);
  const hasInput = !!formattedParams;
  const hasOutput = result != null || !!error || status === 'running';

  return (
    <div className={styles.toolBlock}>
      <div className={styles.toolHeader} onClick={handleToggle}>
        <div className={styles.toolHeaderLeft}>
          <span className={styles.toolIconEmoji}>
            {status === 'completed' ? '✓' : status === 'failed' ? '✗' : '⚙'}
          </span>
          <span className={styles.toolName}>{toolName}</span>
          <StatusIndicator status={statusDisplay} />
        </div>
        <div className={styles.toolHeaderRight}>
          <span className={`${styles.expandIcon} ${isExpanded ? styles.expanded : ''}`}>
            ▼
          </span>
        </div>
      </div>

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
            <>
              {result != null && (
                <div className={styles.toolResult}>
                  {renderToolResult(result)}
                </div>
              )}
              {error && (
                <div className={styles.toolResult}>
                  <span style={{ color: 'var(--color-critical)' }}>{error}</span>
                </div>
              )}
              {!result && !error && status === 'running' && (
                <div className={styles.loadingState}>
                  <span className={styles.spinner}></span>
                  <span>Executing...</span>
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
});

const StatusIndicator = memo(({ status }) => {
  switch (status) {
    case 'pending':
      return (
        <span className={styles.statusPending}>
          <span className={styles.spinner}></span>
          Running...
        </span>
      );
    case 'complete':
      // The leading ✓ icon next to the tool name already conveys success — a
      // separate green pill on every successful call adds noise. Surface a
      // badge only for non-default states (pending, warning, error).
      return null;
    case 'warning':
      return (
        <span className={styles.statusWarning}>
          <span className={styles.warningIcon}>⚠</span>
          Warnings
        </span>
      );
    case 'error':
      return (
        <span className={styles.statusError}>
          <span className={styles.errorIcon}>✗</span>
          Failed
        </span>
      );
    default:
      return null;
  }
});

/**
 * NoticesBanner — expandable banner showing policy warnings for a run
 */
const NoticesBanner = memo(({ notices }) => {
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
            <div key={i} className={styles.noticeItem}>{notice.message}</div>
          ))}
        </div>
      )}
    </div>
  );
});

export { NoticesBanner };
export default ChatMessageList;
