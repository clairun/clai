import React, { useState, useRef, useEffect, useCallback } from 'react';
import { useLocation } from 'react-router-dom';
import WorkspaceContextBar from '../../workspace/components/WorkspaceContextBar';
import CommandHelpModal from './CommandHelpModal';
import WorkspaceTerminal from './WorkspaceTerminal';
import {
  dispatchScrollChatToBottom,
  dispatchWorkspaceUiCommand,
} from '../../utils/workspaceUiEvents';
import styles from './TerminalEmulator.module.css';

type OutputType = 'info' | 'success' | 'error' | 'warning';

interface OutputMessage {
  id: number;
  text: string;
  type: OutputType;
  timestamp: Date;
}

interface SendToChatResult {
  error?: string;
  message?: string;
}

interface TerminalEmulatorProps {
  onSendToChat?: (text: string) => Promise<SendToChatResult | void>;
  onAgentCommand?: (command: string) => Promise<SendToChatResult | void>;
  agentWorking?: boolean;
}

const TerminalEmulator = ({
  onSendToChat,
  onAgentCommand,
  agentWorking = false,
}: TerminalEmulatorProps) => {
  const location = useLocation();
  const [inputValue, setInputValue] = useState('');
  const [outputMessages, setOutputMessages] = useState<OutputMessage[]>([]);
  const [isOutputVisible, setIsOutputVisible] = useState(true);
  const [isHoveringOutput, setIsHoveringOutput] = useState(false);
  const [showHelp, setShowHelp] = useState(false);
  // Terminal mode: when on, the composer is replaced by a real PTY-backed
  // shell (WorkspaceTerminal). Only meaningful on workspace routes.
  const [terminalMode, setTerminalMode] = useState(false);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const outputRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<HTMLDivElement>(null);
  const autoCollapseTimerRef = useRef<number | null>(null);
  // CHANGED: Ref for the wrapper element instead of display to handle scrolling
  const inputWrapperRef = useRef<HTMLDivElement>(null);
  // Consume-once holder for a `!cmd` command: set when the user submits a `!`
  // line, read+cleared by WorkspaceTerminal when its shell is ready, so the
  // command runs exactly once and never replays on a later terminal toggle.
  const pendingCommandRef = useRef<string | null>(null);
  const consumeInitialCommand = useCallback(() => {
    const cmd = pendingCommandRef.current;
    pendingCommandRef.current = null;
    return cmd;
  }, []);

  const isWorkspaceRoute =
    location.pathname === '/workspace' || location.pathname.startsWith('/workspace/');
  const workspaceRouteMatch = location.pathname.match(/^\/workspace\/([^/]+)\/?$/);
  const currentWorkspaceId = workspaceRouteMatch
    ? decodeURIComponent(workspaceRouteMatch[1]!)
    : null;
  const terminalAvailable = isWorkspaceRoute && Boolean(currentWorkspaceId);
  const showTerminal = terminalMode && terminalAvailable;

  // Maximum number of messages to keep
  const MAX_MESSAGES = 5;
  // Auto-collapse delay in milliseconds
  const AUTO_COLLAPSE_DELAY = 10000;

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
    autoCollapseTimerRef.current = window.setTimeout(() => {
      setIsOutputVisible(false);
    }, AUTO_COLLAPSE_DELAY);
  }, [AUTO_COLLAPSE_DELAY]);

  // Helper function to add output messages
  const addOutputMessage = useCallback(
    (message: string, type: OutputType = 'info') => {
      const newMessage: OutputMessage = {
        id: Date.now() + Math.random(),
        text: message,
        type, // 'info', 'success', 'error', 'warning'
        timestamp: new Date(),
      };
      setOutputMessages((prev) => {
        const updated = [...prev, newMessage];
        // Keep only the last MAX_MESSAGES messages
        return updated.slice(-MAX_MESSAGES);
      });
      // Show progress/error/warning messages immediately. Success messages are
      // added to history but don't expand the panel.
      if (type === 'info' || type === 'error' || type === 'warning') {
        setIsOutputVisible(true);
        resetAutoCollapseTimer();
      }
    },
    [resetAutoCollapseTimer, MAX_MESSAGES]
  );

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

  // Leave terminal mode automatically when the workspace context goes away
  // (navigating off a workspace route). Done via the "adjust state during
  // render" pattern (React docs) rather than an effect: the WorkspaceTerminal
  // then unmounts and kills its shell.
  const [prevTerminalAvailable, setPrevTerminalAvailable] = useState(terminalAvailable);
  if (prevTerminalAvailable !== terminalAvailable) {
    setPrevTerminalAvailable(terminalAvailable);
    if (!terminalAvailable) {
      setTerminalMode(false);
    }
  }

  // Ctrl+\ (or Cmd+\) toggles terminal mode. Matches the backslash key
  // (the 'Backslash' code or the '\\' character) and also the physical key
  // in the US-backtick position ('Backquote'): on a Spanish/ISO layout that
  // key is the one users reach for as backslash, and a bare backtick is
  // awkward. Capture phase so it fires even when the xterm grid has focus.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const isToggleKey = e.code === 'Backslash' || e.code === 'Backquote' || e.key === '\\';
      if ((e.ctrlKey || e.metaKey) && !e.altKey && isToggleKey) {
        if (!terminalAvailable) return;
        e.preventDefault();
        e.stopPropagation();
        setTerminalMode((on) => !on);
      }
    };
    document.addEventListener('keydown', handler, true);
    return () => document.removeEventListener('keydown', handler, true);
  }, [terminalAvailable]);

  // Entering terminal mode shrinks the conversation viewport — nudge it back
  // to the bottom (after the reflow settles) so the latest messages stay in
  // view. Leaving terminal mode returns keyboard focus to the chat input so
  // the user can type immediately without clicking.
  const prevShowTerminalRef = useRef(showTerminal);
  useEffect(() => {
    const wasShowing = prevShowTerminalRef.current;
    prevShowTerminalRef.current = showTerminal;
    if (!wasShowing && showTerminal) {
      const timer = window.setTimeout(() => dispatchScrollChatToBottom(), 120);
      return () => window.clearTimeout(timer);
    }
    if (wasShowing && !showTerminal) {
      inputRef.current?.focus();
    }
    return undefined;
  }, [showTerminal]);

  // Handle command execution
  const handleCommandExecution = async (input: string) => {
    const trimmed = input.trim();
    if (!trimmed) return;

    // Clear input immediately and reset textarea height
    setInputValue('');
    resetTextareaHeight();

    // "!cmd" runs a command in the integrated terminal and switches to
    // terminal mode; a bare "!" just opens the terminal. Only on workspace
    // routes (where a shell can open).
    if (trimmed.startsWith('!')) {
      if (!terminalAvailable) {
        addOutputMessage('Open a workspace to run terminal commands.', 'error');
        return;
      }
      pendingCommandRef.current = trimmed.slice(1).trim() || null;
      setTerminalMode(true);
      return;
    }

    // Check if input starts with "/" - it's a command
    const isSlashCommand = trimmed.startsWith('/');

    // If NOT a slash command, send to chat. Workspace routes render the chat
    // in the page; other routes reject with a hint to open a workspace.
    if (!isSlashCommand) {
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

    const commandName = commandInput.split(/\s+/, 1)[0] || '';
    const commandArgs = commandInput.slice(commandName.length).trim();

    if (commandName === 'help') {
      setShowHelp(true);
      return;
    }

    if (commandName === 'compact' || commandName === 'clear') {
      if (!onAgentCommand) {
        addOutputMessage('Assistant commands are not available here.', 'error');
        return;
      }
      const result = await onAgentCommand(commandInput);
      if (result?.error) {
        addOutputMessage(result.error, 'error');
      } else if (result?.message) {
        addOutputMessage(result.message, 'success');
      }
      return;
    }

    // Workspace UI commands — delivered to FleetLayout (which owns the
    // settings modal and the fork flow) via a window event, since the
    // terminal lives in a different React subtree.
    if (commandName === 'settings' || commandName === 'fork') {
      if (!isWorkspaceRoute) {
        addOutputMessage(`Open a workspace to use /${commandName}.`, 'error');
        return;
      }
      // Fork progress is shown as a blocking modal by FleetLayout (driven by
      // its forkBusyId state) — no terminal output, which would otherwise
      // linger as a stale line after the fork completes.
      dispatchWorkspaceUiCommand({
        action: commandName,
        workspaceId: currentWorkspaceId || 'default',
        prompt: commandName === 'fork' ? commandArgs : null,
      });
      return;
    }

    // Anything else is unknown. The legacy /tab, /ctx, /tile and /reset-all
    // commands (and the command-visualization registry they fed) were
    // removed with the old tabs/tiles UI.
    addOutputMessage(
      `Unknown command: /${commandName || commandInput}. Type /help for available commands.`,
      'error'
    );
  };

  // Handle keyboard events
  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter sends message (Shift+Enter for newline)
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleCommandExecution(inputValue);
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
    const handleGlobalKeyDown = (e: KeyboardEvent) => {
      // Check for Ctrl+L (Windows/Linux) or Cmd+L (Mac)
      if ((e.ctrlKey || e.metaKey) && e.key === 'l') {
        e.preventDefault();
        if (inputRef.current) {
          inputRef.current.focus();
        }
      }
    };

    // Add global event listener
    document.addEventListener('keydown', handleGlobalKeyDown);

    // Cleanup on unmount
    return () => {
      document.removeEventListener('keydown', handleGlobalKeyDown);
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
    // In terminal mode the textarea isn't rendered; let the click reach xterm.
    if (inputRef.current) {
      inputRef.current.focus();
    }
  };

  return (
    <div ref={terminalRef} className={styles.terminal} onClick={handleTerminalClick}>
      {showTerminal ? (
        <WorkspaceTerminal
          key={currentWorkspaceId!}
          workspaceId={currentWorkspaceId!}
          consumeInitialCommand={consumeInitialCommand}
          onExit={() => setTerminalMode(false)}
        />
      ) : (
        <>
          {/* Workspace context bar — MCP badges inside the terminal on workspace routes */}
          {isWorkspaceRoute && currentWorkspaceId && (
            <div className={styles.workspaceContextWrapper}>
              <WorkspaceContextBar workspaceId={currentWorkspaceId} />
            </div>
          )}

          {/* Input Line - Now at the top for better UX */}
          <div className={styles.terminalContent}>
            {/* Terminal-mode toggle — only where a workspace shell can open.
                Mirrors the Ctrl+` shortcut. */}
            {terminalAvailable && (
              <button
                type="button"
                className={styles.modeToggle}
                onClick={(e) => {
                  e.stopPropagation();
                  setTerminalMode(true);
                }}
                title="Terminal mode (Ctrl+\)"
                aria-label="Switch to terminal mode"
              >
                {'>_'}
              </button>
            )}

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
                aria-busy={agentWorking || undefined}
                placeholder={
                  agentWorking
                    ? 'Agent is working — Enter queues a follow-up message...'
                    : isWorkspaceRoute
                      ? 'Message this workspace…  (!cmd runs a terminal command)'
                      : 'Open a workspace to chat, or run a /command (/help)...'
                }
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
                <div
                  key={msg.id}
                  className={`${styles.outputMessage} ${styles[`outputMessage${msg.type.charAt(0).toUpperCase() + msg.type.slice(1)}`]}`}
                >
                  <span className={styles.outputMessageText}>{msg.text}</span>
                </div>
              ))}
            </div>
          )}
        </>
      )}

      {showHelp && <CommandHelpModal onClose={() => setShowHelp(false)} />}
    </div>
  );
};

export default TerminalEmulator;
