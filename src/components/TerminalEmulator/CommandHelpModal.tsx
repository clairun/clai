import React, { useEffect } from 'react';
import ReactDOM from 'react-dom';
import styles from './CommandHelpModal.module.css';

interface CommandEntry {
  command: string;
  description: string;
  scope?: string;
}

const COMMANDS: CommandEntry[] = [
  {
    command: '/help',
    description: 'Show this help.',
  },
  {
    command: '/compact',
    description:
      'Summarize older conversation history into a compact note to free up context. The recent messages stay verbatim.',
  },
  {
    command: '/clear',
    description:
      'Delete the entire conversation history (messages, runs, tool calls). Artifacts, memories and tasks survive. Cannot be undone.',
  },
  {
    command: '/settings',
    description: "Open the current workspace's settings.",
    scope: 'workspace',
  },
  {
    command: '/fork <prompt>',
    description: 'Fork the current workspace, switch to it, and optionally start with the prompt.',
    scope: 'workspace',
  },
  {
    command: '!<command>',
    description:
      'Run a command in the integrated terminal (switches to terminal mode). A bare ! just opens the terminal.',
    scope: 'workspace',
  },
];

const SHORTCUTS: { keys: string; description: string }[] = [
  { keys: 'Enter', description: 'Send message (queued if the agent is working)' },
  { keys: 'Shift+Enter', description: 'New line' },
  { keys: 'Ctrl/Cmd+L', description: 'Focus the input' },
  { keys: 'Ctrl/Cmd+\\', description: 'Toggle terminal mode' },
  { keys: 'Ctrl+Shift+C / V', description: 'Copy / paste in the terminal (Cmd+C/V on macOS)' },
];

interface CommandHelpModalProps {
  onClose: () => void;
}

/**
 * CommandHelpModal — the /help overlay listing slash commands and
 * keyboard shortcuts. Portals to <body> so it stacks above the floating
 * terminal card; closes on Escape, backdrop click, or the × button.
 */
const CommandHelpModal = ({ onClose }: CommandHelpModalProps) => {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [onClose]);

  return ReactDOM.createPortal(
    <div className={styles.backdrop} onClick={onClose}>
      <div
        className={styles.modal}
        role="dialog"
        aria-label="Available commands"
        onClick={(event) => event.stopPropagation()}
      >
        <div className={styles.header}>
          <span className={styles.title}>Commands</span>
          <button type="button" className={styles.closeButton} onClick={onClose} aria-label="Close">
            ×
          </button>
        </div>

        <div className={styles.body}>
          <div className={styles.commandList}>
            {COMMANDS.map((entry) => (
              <div key={entry.command} className={styles.commandRow}>
                <code className={styles.commandName}>{entry.command}</code>
                <span className={styles.commandDescription}>
                  {entry.description}
                  {entry.scope && <span className={styles.scopeChip}>{entry.scope}</span>}
                </span>
              </div>
            ))}
          </div>

          <div className={styles.sectionTitle}>Keyboard</div>
          <div className={styles.commandList}>
            {SHORTCUTS.map((entry) => (
              <div key={entry.keys} className={styles.commandRow}>
                <code className={styles.commandName}>{entry.keys}</code>
                <span className={styles.commandDescription}>{entry.description}</span>
              </div>
            ))}
          </div>

          <p className={styles.hint}>Anything that doesn&apos;t start with / is sent to the agent.</p>
        </div>
      </div>
    </div>,
    document.body
  );
};

export default CommandHelpModal;
