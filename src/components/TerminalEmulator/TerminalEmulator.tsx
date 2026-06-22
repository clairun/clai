import React, { useState, useRef, useEffect, useCallback } from 'react';
import { useLocation } from 'react-router-dom';
import WorkspaceContextBar from '../../workspace/components/WorkspaceContextBar';
import CommandHelpModal from './CommandHelpModal';
import WorkspaceTerminal from './WorkspaceTerminal';
import {
  dispatchScrollChatToBottom,
  dispatchWorkspaceUiCommand,
} from '../../utils/workspaceUiEvents';
import type { ContentPart } from '../../generated/bindings';
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

type AttachImageResult = { part?: ContentPart; error?: string };

interface ComposerAttachment {
  part: ContentPart;
  /** Object URL of the source File, for the local thumbnail preview only. */
  previewUrl: string;
}

interface TerminalEmulatorProps {
  onSendToChat?: (text: string, images: ContentPart[]) => Promise<SendToChatResult | void>;
  onAgentCommand?: (command: string) => Promise<SendToChatResult | void>;
  onAttachImage?: (file: File) => Promise<AttachImageResult>;
  onPickImage?: () => Promise<AttachImageResult>;
  onReadClipboardImage?: () => Promise<File | null>;
  agentWorking?: boolean;
}

const TerminalEmulator = ({
  onSendToChat,
  onAgentCommand,
  onAttachImage,
  onPickImage,
  onReadClipboardImage,
  agentWorking = false,
}: TerminalEmulatorProps) => {
  const location = useLocation();
  const [inputValue, setInputValue] = useState('');
  const [attachments, setAttachments] = useState<ComposerAttachment[]>([]);
  const [isAttaching, setIsAttaching] = useState(false);
  const [outputMessages, setOutputMessages] = useState<OutputMessage[]>([]);
  const [isOutputVisible, setIsOutputVisible] = useState(true);
  const [isHoveringOutput, setIsHoveringOutput] = useState(false);
  const [showHelp, setShowHelp] = useState(false);
  // Terminal mode: when on, the composer is replaced by a real PTY-backed
  // shell (WorkspaceTerminal). Only meaningful on workspace routes.
  const [terminalMode, setTerminalMode] = useState(false);
  // Workspaces that currently have a live (kept-alive) terminal. Each is
  // rendered mounted-but-hidden until its workspace is active, so its PTY and
  // screen persist across navigation for the whole app session. Entries are
  // removed only when the shell exits (see onShellExit in the render).
  const [openedTerminals, setOpenedTerminals] = useState<string[]>([]);
  // Workspaces whose terminal is maximized (fullscreen fills the detail pane and
  // keeps the left rail). Per-workspace so each terminal reopens the way it was
  // left — mirroring the kept-alive model above — and persists across chat<->
  // terminal toggles and workspace switches. Cleared on shell exit. Session-only.
  const [fullscreenWorkspaces, setFullscreenWorkspaces] = useState<string[]>([]);
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
  // Show the terminal only when the active workspace actually has a live
  // (kept-alive) terminal. This also recovers gracefully if a workspace's shell
  // exited while it was in the background: its saved terminalMode may still be
  // true, but with no live terminal we fall back to the composer instead of
  // rendering a blank pane. enterTerminalMode registers + enables together, so
  // normal entry still shows immediately.
  const showTerminal =
    terminalMode &&
    terminalAvailable &&
    !!currentWorkspaceId &&
    openedTerminals.includes(currentWorkspaceId);
  // Mirror of showTerminal for the capture-phase Ctrl+\\ listener, which must
  // toggle off what is actually on screen rather than the saved terminalMode
  // flag: after a background shell exits, terminalMode can stay true while the
  // composer is shown, and keying off the stale flag would need two presses.
  const showTerminalRef = useRef(false);
  useEffect(() => {
    showTerminalRef.current = showTerminal;
  }, [showTerminal]);
  // Whether the active workspace's terminal is maximized (derived per workspace).
  const fullscreen =
    !!currentWorkspaceId && fullscreenWorkspaces.includes(currentWorkspaceId);

  // Enter terminal mode for the current workspace AND register it in the
  // kept-alive set so its shell persists for the rest of the app session. The
  // single entry point for every "open the terminal" affordance (>_ button,
  // Ctrl+\\, and the !cmd fast-path), so registration can never mis-fire on a
  // workspace-switch render where terminalMode is mid-update.
  const enterTerminalMode = useCallback(() => {
    if (!terminalAvailable || !currentWorkspaceId) return;
    setOpenedTerminals((prev) =>
      prev.includes(currentWorkspaceId) ? prev : [...prev, currentWorkspaceId]
    );
    setTerminalMode(true);
    // No fullscreen reset here: a workspace reopens in whatever mode it was last
    // left (docked or maximized), per the fullscreenWorkspaces set.
  }, [terminalAvailable, currentWorkspaceId]);

  // Toggle the active workspace's maximized state (button + Ctrl/Cmd+Shift+Enter).
  const toggleFullscreen = useCallback(() => {
    if (!currentWorkspaceId) return;
    setFullscreenWorkspaces((prev) =>
      prev.includes(currentWorkspaceId)
        ? prev.filter((id) => id !== currentWorkspaceId)
        : [...prev, currentWorkspaceId]
    );
  }, [currentWorkspaceId]);

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

  // Per-workspace composer state. The composer is a single global instance
  // (mounted once in MainLayout), so without this its draft text and terminal
  // mode would leak across workspace switches: a draft typed in workspace A
  // would appear in B, and a terminal opened in A would show in B too. We
  // snapshot the outgoing workspace's draft + terminal mode and restore the
  // incoming one's (empty draft / chat mode by default). Keyed by workspace
  // id; the empty-string key buckets non-workspace routes. Persists for the
  // app session, not across restarts.
  //
  // Done with the "adjust state during render" pattern (React docs) so the swap
  // lands before paint (no stale flash). This also subsumes the old "leave
  // terminal mode when the workspace context goes away" logic: navigating off a
  // workspace switches to the empty key, which defaults terminal mode off (and
  // `showTerminal` also guards on `terminalAvailable`).
  const composerKey = currentWorkspaceId ?? '';
  const [savedComposerState, setSavedComposerState] = useState<{
    activeKey: string;
    drafts: Map<string, string>;
    terminalModes: Map<string, boolean>;
    // activeKey starts as the first-mounted route's key; the swap below keeps
    // it in lockstep with the current route thereafter.
  }>(() => ({ activeKey: composerKey, drafts: new Map(), terminalModes: new Map() }));
  if (savedComposerState.activeKey !== composerKey) {
    const drafts = new Map(savedComposerState.drafts).set(savedComposerState.activeKey, inputValue);
    const terminalModes = new Map(savedComposerState.terminalModes).set(
      savedComposerState.activeKey,
      terminalMode
    );
    setInputValue(drafts.get(composerKey) ?? '');
    setTerminalMode(terminalModes.get(composerKey) ?? false);
    setSavedComposerState({ activeKey: composerKey, drafts, terminalModes });
    // Pending attachments belong to the workspace they were attached in (their
    // stored paths are relative to that workspace's root). Drop them on switch,
    // otherwise a send in the new workspace ships a path that resolves against
    // the wrong root and the image is silently lost. Revoke is idempotent.
    if (attachments.length > 0) {
      attachments.forEach((a) => URL.revokeObjectURL(a.previewUrl));
      setAttachments([]);
    }
  }

  // Ctrl+\ (or Cmd+\) toggles terminal mode. Matches the backslash key
  // (the 'Backslash' code or the '\\' character) and also the physical key
  // in the US-backtick position ('Backquote'): on a Spanish/ISO layout that
  // key is the one users reach for as backslash, and a bare backtick is
  // awkward. Capture phase so it fires even when the xterm grid has focus.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // Ctrl/Cmd+Shift+Enter toggles fullscreen while the terminal is shown.
      // (No Esc binding: TUIs like vim need Esc, and this listener is capture
      // phase, so it would steal it from the shell.)
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key === 'Enter') {
        if (!showTerminalRef.current) return;
        e.preventDefault();
        e.stopPropagation();
        toggleFullscreen();
        return;
      }
      const isToggleKey = e.code === 'Backslash' || e.code === 'Backquote' || e.key === '\\';
      if ((e.ctrlKey || e.metaKey) && !e.altKey && isToggleKey) {
        if (!terminalAvailable) return;
        e.preventDefault();
        e.stopPropagation();
        if (showTerminalRef.current) setTerminalMode(false);
        else enterTerminalMode();
      }
    };
    document.addEventListener('keydown', handler, true);
    return () => document.removeEventListener('keydown', handler, true);
  }, [terminalAvailable, enterTerminalMode, toggleFullscreen]);

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
  const removeAttachment = (index: number) => {
    setAttachments((prev) => {
      const target = prev[index];
      if (target) URL.revokeObjectURL(target.previewUrl);
      return prev.filter((_, i) => i !== index);
    });
  };

  const clearAttachments = useCallback(() => {
    setAttachments((prev) => {
      prev.forEach((a) => URL.revokeObjectURL(a.previewUrl));
      return [];
    });
  }, []);

  // Revoke any outstanding preview URLs on unmount.
  useEffect(() => clearAttachments, [clearAttachments]);

  const attachFiles = async (files: File[]) => {
    if (!onAttachImage || files.length === 0) return;
    setIsAttaching(true);
    try {
      for (const file of files) {
        const res = await onAttachImage(file);
        if (res.error) {
          addOutputMessage(res.error, 'error');
        } else if (res.part) {
          const previewUrl = URL.createObjectURL(file);
          setAttachments((prev) => [...prev, { part: res.part as ContentPart, previewUrl }]);
        }
      }
    } finally {
      setIsAttaching(false);
    }
  };

  const handlePaste = async (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    // Chromium/WebView2 (Windows) surfaces pasted images here.
    const files = Array.from(e.clipboardData?.items ?? [])
      .filter((it) => it.kind === 'file' && it.type.startsWith('image/'))
      .map((it) => it.getAsFile())
      .filter((f): f is File => f !== null);
    if (files.length > 0) {
      e.preventDefault();
      await attachFiles(files);
      return;
    }
    // WebKit (Linux/mac) doesn't expose pasted images to the DOM paste event;
    // fall back to reading the native OS clipboard. A non-image clipboard
    // returns null, so the ordinary text paste proceeds untouched.
    if (!onReadClipboardImage) return;
    const nativeFile = await onReadClipboardImage();
    if (nativeFile) await attachFiles([nativeFile]);
  };

  // Native file-picker attach (reliable everywhere; paste is flaky on WebKitGTK).
  const handlePickImage = async () => {
    if (!onPickImage) return;
    setIsAttaching(true);
    try {
      const res = await onPickImage();
      if (res.error) {
        addOutputMessage(res.error, 'error');
      } else if (res.part) {
        // No object URL: the picked file lives on disk, not as a browser File.
        setAttachments((prev) => [...prev, { part: res.part as ContentPart, previewUrl: '' }]);
      }
    } finally {
      setIsAttaching(false);
    }
  };

  const handleCommandExecution = async (input: string) => {
    const trimmed = input.trim();
    const pendingImages = attachments.map((a) => a.part);
    // Allow an image-only send (no text) when attachments are present.
    if (!trimmed && pendingImages.length === 0) return;

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
      enterTerminalMode();
      return;
    }

    // Check if input starts with "/" - it's a command
    const isSlashCommand = trimmed.startsWith('/');

    // If NOT a slash command, send to chat. Workspace routes render the chat
    // in the page; other routes reject with a hint to open a workspace.
    if (!isSlashCommand) {
      if (onSendToChat) {
        const result = await onSendToChat(trimmed, pendingImages);
        if (result?.error) {
          addOutputMessage(result.error, 'error');
        } else {
          // Clear only on success — a failed turn keeps the image attached so
          // the user can retry without re-picking it.
          clearAttachments();
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
    // Clicking the composer area focuses its textarea. In terminal mode the
    // composer is hidden (display:none) and xterm owns focus, so skip it.
    if (!showTerminal) inputRef.current?.focus();
  };

  return (
    <div
      ref={terminalRef}
      className={`${styles.terminal} ${
        showTerminal && fullscreen ? styles.terminalFullscreen : ''
      }`}
      onClick={handleTerminalClick}
    >
      {/* Kept-alive terminals: one per workspace that has opened a terminal.
          They stay mounted for the whole app session so the PTY *and* the
          rendered screen (vim, build logs, scrollback) survive navigation;
          only the active workspace's terminal is visible. */}
      {openedTerminals.map((wsId) => (
        <WorkspaceTerminal
          key={wsId}
          workspaceId={wsId}
          visible={showTerminal && wsId === currentWorkspaceId}
          consumeInitialCommand={consumeInitialCommand}
          fullscreen={fullscreen && wsId === currentWorkspaceId}
          onToggleFullscreen={toggleFullscreen}
          onBackToChat={() => setTerminalMode(false)}
          onShellExit={() => {
            setOpenedTerminals((prev) => prev.filter((id) => id !== wsId));
            setFullscreenWorkspaces((prev) => prev.filter((id) => id !== wsId));
            if (wsId === currentWorkspaceId) {
              setTerminalMode(false);
            }
          }}
        />
      ))}

      {/* Composer: shown when not in terminal mode. `display: contents` keeps
          its children laid out as direct flex items of `.terminal` (no extra
          box); it is hidden — not unmounted — while the terminal is on screen,
          so composer state is preserved too. */}
      <div style={{ display: showTerminal ? 'none' : 'contents' }}>
          {/* Workspace context bar — MCP badges inside the terminal on workspace routes */}
          {isWorkspaceRoute && currentWorkspaceId && (
            <div className={styles.workspaceContextWrapper}>
              <WorkspaceContextBar workspaceId={currentWorkspaceId} />
            </div>
          )}

          {/* Input Line - Now at the top for better UX */}
          <div className={styles.terminalContent}>
            {/* Pasted/attached image thumbnails, above the input */}
            {(attachments.length > 0 || isAttaching) && (
              <div className={styles.attachmentTray} aria-label="Image attachments">
                {attachments.map((att, index) => (
                  <div
                    key={att.part.type === 'image' ? att.part.id : index}
                    className={styles.attachmentThumb}
                  >
                    {att.previewUrl ? (
                      <img
                        src={att.previewUrl}
                        alt={
                          att.part.type === 'image' && att.part.filename
                            ? att.part.filename
                            : 'Attached image'
                        }
                      />
                    ) : (
                      <span className={styles.attachmentChip}>
                        {att.part.type === 'image' && att.part.filename
                          ? att.part.filename
                          : 'image'}
                      </span>
                    )}
                    <button
                      type="button"
                      className={styles.attachmentRemove}
                      onClick={(e) => {
                        e.stopPropagation();
                        removeAttachment(index);
                      }}
                      aria-label="Remove image"
                      title="Remove image"
                    >
                      ×
                    </button>
                  </div>
                ))}
                {isAttaching && <span className={styles.attachmentSpinner}>Attaching…</span>}
              </div>
            )}

            {/* Composer input box */}
            <div className={styles.terminalInputWrapper} ref={inputWrapperRef}>
              <textarea
                ref={inputRef}
                rows={1}
                className={styles.terminalInput}
                value={inputValue}
                onChange={(e) => setInputValue(e.target.value)}
                onKeyDown={handleKeyDown}
                onPaste={handlePaste}
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

            {/* Action toolbar: attach + terminal-mode, below the input */}
            {((onPickImage && isWorkspaceRoute) || terminalAvailable) && (
              <div className={styles.composerToolbar}>
                {onPickImage && isWorkspaceRoute && (
                  <button
                    type="button"
                    className={styles.attachButton}
                    onClick={(e) => {
                      e.stopPropagation();
                      void handlePickImage();
                    }}
                    disabled={isAttaching}
                    aria-label="Attach image"
                    title="Attach image"
                  >
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="2"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                      aria-hidden="true"
                    >
                      <path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48" />
                    </svg>
                  </button>
                )}
                {terminalAvailable && (
                  <button
                    type="button"
                    className={styles.modeToggle}
                    onClick={(e) => {
                      e.stopPropagation();
                      enterTerminalMode();
                    }}
                    title="Terminal mode (Ctrl+\)"
                    aria-label="Switch to terminal mode"
                  >
                    {'>_'}
                  </button>
                )}
              </div>
            )}
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
      </div>

      {showHelp && <CommandHelpModal onClose={() => setShowHelp(false)} />}
    </div>
  );
};

export default TerminalEmulator;
