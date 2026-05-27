import React, { useEffect, useState, useCallback } from 'react';
import { assistantClient, useAssistantSession } from '../../assistant';
import useAssistantStore from '../../assistant/sessionStore';
import type { ToolInvocation } from '../../generated/bindings';
import ChatMessageList from './ChatMessageList';
import styles from './AssistantChat.module.css';

const EMPTY_TOOL_CALLS: ToolInvocation[] = [];
const CONNECTIONS_CHANGED_EVENT = 'assistant-provider-connections-changed';

interface AssistantChatProps {
  tabId: string;
}

/**
 * AssistantChat Component
 *
 * Displays assistant session messages for a specific tab.
 * Sources data from the assistant Zustand store.
 */
const AssistantChat = ({ tabId }: AssistantChatProps) => {
  const {
    messages,
    streamingText,
    isStreaming,
    sessionId,
    clearSessions,
  } = useAssistantSession(tabId);
  const toolCalls = useAssistantStore((state) =>
    sessionId
      ? (state.sessions[sessionId]?.toolCalls || EMPTY_TOOL_CALLS)
      : EMPTY_TOOL_CALLS
  );
  const [providerConfigured, setProviderConfigured] = useState(true);
  const [isClearing, setIsClearing] = useState(false);

  useEffect(() => {
    let cancelled = false;

    const loadProviderStatus = async () => {
      try {
        const sessions = await assistantClient.listProviderConnections();
        if (!cancelled) {
          setProviderConfigured(sessions.some((connection) => connection.enabled));
        }
      } catch {
        if (!cancelled) {
          setProviderConfigured(false);
        }
      }
    };

    loadProviderStatus();
    window.addEventListener(CONNECTIONS_CHANGED_EVENT, loadProviderStatus);

    return () => {
      cancelled = true;
      window.removeEventListener(CONNECTIONS_CHANGED_EVENT, loadProviderStatus);
    };
  }, [sessionId]);

  const handleClearChat = useCallback(async () => {
    if (!tabId || isClearing) return;

    const confirmed = window.confirm(
      'Clear the assistant chat history for this tab? This will remove all saved assistant sessions for the tab.'
    );
    if (!confirmed) return;

    setIsClearing(true);
    try {
      await clearSessions();
    } catch (error) {
      console.error('[AssistantChat] Failed to clear chat:', error);
    } finally {
      setIsClearing(false);
    }
  }, [tabId, isClearing, clearSessions]);

  // Empty state
  if (messages.length === 0) {
    return (
      <div className={styles.agentChat}>
        <div className={styles.chatToolbar}>
          <div className={styles.toolbarTitle}>Clai</div>
          <button
            type="button"
            className={styles.chatActionButton}
            onClick={handleClearChat}
            disabled={isClearing}
          >
            {isClearing ? 'Clearing…' : 'Clear Chat'}
          </button>
        </div>
        <div className={styles.emptyState}>
          <div className={styles.emptyTitle}>No activity yet</div>
          <div className={styles.emptyDescription}>
            {providerConfigured
              ? 'Type a message in the terminal to start a conversation.'
              : 'Connect an assistant provider in Settings, then send a message from the terminal.'}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.agentChat}>
      <div className={styles.chatToolbar}>
        <div className={styles.toolbarTitle}>Clai</div>
        <button
          type="button"
          className={styles.chatActionButton}
          onClick={handleClearChat}
          disabled={isClearing}
        >
          {isClearing ? 'Clearing…' : 'Clear Chat'}
        </button>
      </div>
      <ChatMessageList
        messages={messages}
        streamingText={streamingText}
        isStreaming={isStreaming}
        toolCalls={toolCalls}
      />
    </div>
  );
};

export default AssistantChat;
