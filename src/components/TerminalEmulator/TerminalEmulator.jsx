import React, { useState, useRef, useEffect, useContext, useCallback } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';
import { useCommand } from '../../contexts/CommandContext';
import { useTabManager } from '../../contexts/TabManagerContext';
import { useChatManager } from '../../contexts/ChatManagerContext';
import TabContext from '../../contexts/TabContext';
import { parseCommand, isLayoutCommand } from '../../utils/commandParser';
import { handleContextCommand, isContextCommand } from '../../utils/contextCommandHandler';
import { isCommandSupported } from '../../utils/commandRegistry';
import { createWorkspace } from '../../workspace/client';
import WorkspaceContextBar from '../../workspace/components/WorkspaceContextBar';
import ContextPanel from '../ContextPanel/ContextPanel';
import { SettingsModal } from '../Settings';
import styles from './TerminalEmulator.module.css';

const TerminalEmulator = ({ onSendToChat, disabled = false }) => {
  const { executeCommand, commandHistory } = useCommand();
  const { handleLayoutCommand, getActiveTab } = useTabManager();
  const { setActiveContext, openChat, isCurrentChatOpen } = useChatManager();
  const location = useLocation();
  const navigate = useNavigate();
  // Try to get tab context, but don't throw error if not available
  const tabContext = useContext(TabContext);
  const [inputValue, setInputValue] = useState('');
  const [historyIndex, setHistoryIndex] = useState(-1);
  const [outputMessages, setOutputMessages] = useState([]);
  const [isOutputVisible, setIsOutputVisible] = useState(true);
  const [isHoveringOutput, setIsHoveringOutput] = useState(false);
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const inputRef = useRef(null);
  const outputRef = useRef(null);
  const terminalRef = useRef(null);
  const autoCollapseTimerRef = useRef(null);
  // CHANGED: Ref for the wrapper element instead of display to handle scrolling
  const inputWrapperRef = useRef(null);

  // Check if desktop chat panel is open
  const isChatOpen = isCurrentChatOpen();
  const isFleetRoute = location.pathname === '/fleet';
  const isWorkspaceRoute = location.pathname === '/workspace' || location.pathname.startsWith('/workspace/');
  const workspaceRouteMatch = location.pathname.match(/^\/workspace\/([^/]+)\/?$/);
  const currentWorkspaceId = workspaceRouteMatch ? decodeURIComponent(workspaceRouteMatch[1]) : null;
  // Hide ContextPanel on Fleet and workspace routes (workspace has its own context bar)
  const hideContextPanel = isFleetRoute || isWorkspaceRoute;

  // Maximum number of messages to keep
  const MAX_MESSAGES = 5;
  // Auto-collapse delay in milliseconds
  const AUTO_COLLAPSE_DELAY = 5000; // 10 seconds

  // Auto-resize textarea to fit content
  const adjustTextareaHeight = useCallback(() => {
    const textarea = inputRef.current;
    if (textarea) {
      // Reset height to measure content
      textarea.style.height = '20px';
      // Only grow if content exceeds single line
      const scrollHeight = textarea.scrollHeight;
      const lineHeight = 20;
      const maxHeight = 150; // ~6 lines
      if (scrollHeight > lineHeight) {
        textarea.style.height = `${Math.min(scrollHeight, maxHeight)}px`;
      }
    }
  }, []);

  // Adjust height when input value changes
  useEffect(() => {
    adjustTextareaHeight();
  }, [inputValue, adjustTextareaHeight]);

  // Reset textarea height to single line
  const resetTextareaHeight = useCallback(() => {
    const textarea = inputRef.current;
    if (textarea) {
      textarea.style.height = 'auto';
    }
  }, []);

  // Reset auto-collapse timer - memoized to avoid recreating on every render
  const resetAutoCollapseTimer = useCallback(() => {
    // Clear existing timer
    if (autoCollapseTimerRef.current) {
      clearTimeout(autoCollapseTimerRef.current);
    }
    // Set new timer
    autoCollapseTimerRef.current = setTimeout(() => {
      setIsOutputVisible(false);
    }, AUTO_COLLAPSE_DELAY);
  }, [AUTO_COLLAPSE_DELAY]);

  // Helper function to add output messages
  const addOutputMessage = useCallback((message, type = 'info') => {
    const newMessage = {
      id: Date.now() + Math.random(),
      text: message,
      type, // 'info', 'success', 'error', 'warning'
      timestamp: new Date()
    };
    setOutputMessages(prev => {
      const updated = [...prev, newMessage];
      // Keep only the last MAX_MESSAGES messages
      return updated.slice(-MAX_MESSAGES);
    });
    // Only show output area and reset auto-collapse timer for error/warning messages
    // Success messages are added to the list but don't expand the panel
    if (type === 'error' || type === 'warning') {
      setIsOutputVisible(true);
      resetAutoCollapseTimer();
    }
  }, [resetAutoCollapseTimer, MAX_MESSAGES]);

  // Clear auto-collapse timer on unmount
  useEffect(() => {
    return () => {
      if (autoCollapseTimerRef.current) {
        clearTimeout(autoCollapseTimerRef.current);
      }
    };
  }, []);

  // Handle hover state changes - restart timer when user stops hovering
  useEffect(() => {
    if (!isHoveringOutput && isOutputVisible && outputMessages.length > 0) {
      resetAutoCollapseTimer();
    } else if (isHoveringOutput && autoCollapseTimerRef.current) {
      // Clear timer while hovering to prevent collapse
      clearTimeout(autoCollapseTimerRef.current);
      autoCollapseTimerRef.current = null;
    }
  }, [isHoveringOutput, isOutputVisible, outputMessages.length, resetAutoCollapseTimer]);

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight;
    }
  }, [outputMessages]);

  // Sync chat visibility with the active tab
  useEffect(() => {
    const activeTab = getActiveTab();
    if (activeTab?.id) {
      setActiveContext(activeTab.id, 'tab');
    }
  }, [getActiveTab, setActiveContext]);

  // Handle command execution
  const handleCommandExecution = async (input) => {
    const trimmed = input.trim();
    if (!trimmed) return;

    // Chat is disabled while the active session has a run in flight. Slash
    // commands stay live (they're local UI actions, not LLM turns) — so
    // we only gate the free-text chat path. The server has a matching
    // guard in `assistant_send_message`; this is the visual half.
    if (disabled && !trimmed.startsWith('/')) {
      addOutputMessage(
        'The agent is still working on the previous turn — wait for it to finish.',
        'warning',
      );
      return;
    }

    // Clear input immediately and reset textarea height
    setInputValue('');
    setHistoryIndex(-1);
    resetTextareaHeight();

    // Check if input starts with "/" - it's a command
    const isSlashCommand = trimmed.startsWith('/');

    // If NOT a slash command, send to chat.
    // Auto-open sidebar chat on non-Fleet, non-workspace routes.
    // Workspace routes handle chat opening themselves (agent workspaces open the
    // side panel, general workspaces embed chat in the page).
    if (!isSlashCommand) {
      if (!isChatOpen && !isFleetRoute && !isWorkspaceRoute) {
        openChat();
      }
      if (onSendToChat) {
        const result = await onSendToChat(trimmed);
        if (result?.error) {
          addOutputMessage(result.error, 'error');
        }
      }
      return;
    }

    // Strip the leading "/" and parse as command
    const commandInput = trimmed.slice(1);

    // Parse the command
    const command = parseCommand(commandInput);

    // Check if it's a context command (ctx)
    if (isContextCommand(command)) {
      // Check if TabContext is available
      if (!tabContext) {
        addOutputMessage('Context commands are not available yet. Please wait for the tab to load.', 'error');
        return;
      }

      try {
        const result = await handleContextCommand(command, tabContext);
        if (result.success) {
          addOutputMessage(result.message, 'success');
        } else {
          addOutputMessage(result.message, 'error');
        }
      } catch (error) {
        addOutputMessage(`Context command error: ${error.message}`, 'error');
      }
    }
    // Check if it's a layout command (tab/tile management)
    else if (isLayoutCommand(command)) {
      const result = handleLayoutCommand(command);
      if (result && !result.success) {
        addOutputMessage(result.message || 'Layout command failed', 'error');
      } else if (result && result.message) {
        addOutputMessage(result.message, 'success');
      }
    }
    // Execute visualization/action command with CommandContext
    else {
      // Validate command type using registry - no hardcoded list needed!
      if (!isCommandSupported(command.type)) {
        addOutputMessage(`Unknown command: /${command.type}. Type /help for available commands.`, 'error');
        return;
      }

      executeCommand(command);
    }
  };

  // Handle keyboard events
  const handleKeyDown = (e) => {
    // Enter sends message (Shift+Enter for newline)
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleCommandExecution(inputValue);
    }
    // Ctrl+P: Navigate command history backwards (like bash)
    else if (e.ctrlKey && e.key === 'p') {
      e.preventDefault();
      if (commandHistory.length > 0) {
        const newIndex = historyIndex === -1
          ? commandHistory.length - 1
          : Math.max(0, historyIndex - 1);
        setHistoryIndex(newIndex);
        setInputValue(commandHistory[newIndex]?.raw || '');
      }
    }
    // Ctrl+N: Navigate command history forwards (like bash)
    else if (e.ctrlKey && e.key === 'n') {
      e.preventDefault();
      if (historyIndex !== -1) {
        const newIndex = historyIndex + 1;
        if (newIndex >= commandHistory.length) {
          setHistoryIndex(-1);
          setInputValue('');
        } else {
          setHistoryIndex(newIndex);
          setInputValue(commandHistory[newIndex]?.raw || '');
        }
      }
    }
    // Escape: Clear output
    else if (e.key === 'Escape') {
      setOutputMessages([]);
    }
  };

  // Focus input on mount
  useEffect(() => {
    if (inputRef.current) {
      inputRef.current.focus();
    }
  }, []);

  // Global keyboard shortcut to focus terminal (Ctrl+L or Cmd+L)
  useEffect(() => {
    const handleKeyDown = (e) => {
      // Check for Ctrl+L (Windows/Linux) or Cmd+L (Mac)
      if ((e.ctrlKey || e.metaKey) && e.key === 'l') {
        e.preventDefault();
        if (inputRef.current) {
          inputRef.current.focus();
        }
      }
    };

    // Add global event listener
    document.addEventListener('keydown', handleKeyDown);

    // Cleanup on unmount
    return () => {
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, []);

  // Update CSS variable when terminal height changes (for chat panel positioning)
  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) return;

    const resizeObserver = new ResizeObserver(() => {
      // Use offsetHeight to include padding and borders
      const height = terminal.offsetHeight;
      document.documentElement.style.setProperty('--terminal-height', `${height}px`);
    });

    resizeObserver.observe(terminal);
    return () => resizeObserver.disconnect();
  }, []);

  const handleTerminalClick = () => {
    if (inputRef.current) {
      inputRef.current.focus();
    }
  };

  // Handle settings menu click
  const handleSettingsClick = () => {
    setIsSettingsOpen(true);
  };

  const handleModeToggle = async (event) => {
    event.stopPropagation();
    if (isFleetRoute) {
      // Create a new workspace and navigate to it
      try {
        const id = await createWorkspace();
        navigate(`/workspace/${id}`, { state: { skipFleetRedirect: true } });
      } catch (err) {
        console.error('[TerminalEmulator] Failed to create workspace:', err);
      }
    } else {
      navigate('/fleet');
    }
  };

  return (
    <div ref={terminalRef} className={`${styles.terminal} ${isChatOpen ? styles.chatOpen : ''}`} onClick={handleTerminalClick}>
      {/* Context Panel - shows capability badges (hidden on Fleet and workspace routes) */}
      {!hideContextPanel && (
        <div className={styles.contextPanelWrapper}>
          <ContextPanel />
        </div>
      )}

      {/* Workspace context bar — MCP badges inside the terminal on workspace routes */}
      {isWorkspaceRoute && currentWorkspaceId && (
        <div className={styles.workspaceContextWrapper}>
          <WorkspaceContextBar workspaceId={currentWorkspaceId} />
        </div>
      )}

      {/* Input Line - Now at the top for better UX */}
      <div className={styles.terminalContent}>
        <button
          type="button"
          className={styles.modeButton}
          onClick={handleModeToggle}
          title={isFleetRoute ? 'New workspace' : 'Open fleet'}
          aria-label={isFleetRoute ? 'New workspace' : 'Open fleet'}
        >
          <span className={styles.modeButtonIcon} aria-hidden="true">
            {isFleetRoute ? (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <line x1="12" y1="5" x2="12" y2="19" />
                <line x1="5" y1="12" x2="19" y2="12" />
              </svg>
            ) : (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M17 18a2 2 0 1 0-2-2 2 2 0 0 0 2 2Z" />
                <path d="M7 18a2 2 0 1 0-2-2 2 2 0 0 0 2 2Z" />
                <path d="M12 8a2 2 0 1 0-2-2 2 2 0 0 0 2 2Z" />
                <path d="M7 14v-1a5 5 0 0 1 10 0v1" />
              </svg>
            )}
          </span>
          <span className={styles.modeButtonLabel}>{isFleetRoute ? 'New Workspace' : 'Fleet'}</span>
        </button>

        {/* Global Settings is only accessible from outside a workspace
            (Fleet view). Inside a workspace, configuration belongs to the
            workspace itself and is reached via the workspace's own settings UI. */}
        {!isWorkspaceRoute && (
          <button
            type="button"
            className={styles.settingsButton}
            onClick={(e) => {
              e.stopPropagation();
              handleSettingsClick();
            }}
            title="Open settings"
            aria-label="Open settings"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M12 1v4M12 19v4M4.22 4.22l2.83 2.83M16.95 16.95l2.83 2.83M1 12h4M19 12h4M4.22 19.78l2.83-2.83M16.95 7.05l2.83-2.83" />
            </svg>
          </button>
        )}

        {/* Terminal Prompt Symbol */}
        <span className={styles.terminalPrompt}>%</span>

        {/* Terminal Input - Auto-growing textarea */}
        <div className={styles.terminalInputWrapper} ref={inputWrapperRef}>
          <textarea
            ref={inputRef}
            rows={1}
            className={styles.terminalInput}
            value={inputValue}
            onChange={(e) => setInputValue(e.target.value)}
            onKeyDown={handleKeyDown}
            onClick={(e) => e.stopPropagation()}
            aria-disabled={disabled || undefined}
            placeholder={disabled
              ? 'Agent is working — input will re-enable when the run completes…'
              : isFleetRoute
                ? 'Message the selected agent...'
                : isWorkspaceRoute
                  ? 'Message this workspace...'
                  : 'Type to chat, or /help for commands...'}
            spellCheck={false}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
          />
        </div>

      </div>

      {/* Output Messages Area - Now BELOW the input for better UX */}
      {outputMessages.length > 0 && (
        <div
          className={`${styles.outputArea} ${!isOutputVisible ? styles.outputAreaCollapsed : ''}`}
          ref={outputRef}
          onMouseEnter={() => setIsHoveringOutput(true)}
          onMouseLeave={() => setIsHoveringOutput(false)}
        >
          {outputMessages.map((msg) => (
            <div key={msg.id} className={`${styles.outputMessage} ${styles[`outputMessage${msg.type.charAt(0).toUpperCase() + msg.type.slice(1)}`]}`}>
              <span className={styles.outputMessageText}>{msg.text}</span>
            </div>
          ))}
        </div>
      )}

      {/* Settings Modal */}
      <SettingsModal
        isOpen={isSettingsOpen}
        onClose={() => setIsSettingsOpen(false)}
      />
    </div>
  );
};

export default TerminalEmulator;
