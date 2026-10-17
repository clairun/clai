import React, { useEffect, useRef, useState } from 'react';
import useAssistantStore, { type PendingAskUser } from '../../assistant/sessionStore';
import { OTHER_INDEX, useAskAnswer } from './useAskAnswer';
import styles from './AskUserPanel.module.css';

/**
 * Inline answer block for the `ask_user` tool.
 *
 * Renders when the active workspace session has a pending ask_user
 * request (the run is blocked waiting for the user). Mirrors the
 * AskUserQuestion UX in Claude Code itself: question text, optional
 * structured choices as radio buttons with an auto-added "Other"
 * free-text fallback, or a plain textarea when no options are provided.
 * The draft and its submission live in `useAskAnswer`.
 */
interface AskUserPanelProps {
  sessionId?: string | null;
  /** Answer this question, held by the caller, instead of the session's; the panel doesn't take focus. */
  ask?: PendingAskUser;
}

const AskUserPanel = ({ sessionId = null, ask }: AskUserPanelProps) => {
  const sessionPending = useAssistantStore((state) =>
    sessionId ? state.sessions[sessionId]?.pendingAskUser || null : null
  );
  const pending = ask ?? sessionPending;
  const {
    options,
    hasOptions,
    isMultiSelect,
    selectedIndex,
    setSelectedIndex,
    selectedIndexes,
    setSelectedIndexes,
    otherChecked,
    setOtherChecked,
    otherText,
    setOtherText,
    submitting,
    error,
    canSubmit,
    submit,
  } = useAskAnswer(pending);
  // Collapse the body to a one-line summary so a long question can't swallow
  // the conversation; the user expands to answer. Resets per new request.
  const [collapsed, setCollapsed] = useState(false);
  const containerRef = useRef<HTMLElement | null>(null);
  const previousPendingIdRef = useRef<string | null>(null);

  useEffect(() => {
    if (pending?.pendingId === previousPendingIdRef.current) return;
    previousPendingIdRef.current = pending?.pendingId || null;
    setCollapsed(false);
  }, [pending?.pendingId]);

  // Focus the panel when a request appears so keyboard users land
  // directly on the prompt without having to tab through the chat.
  useEffect(() => {
    if (pending && !ask && containerRef.current) {
      containerRef.current.focus({ preventScroll: false });
    }
  }, [pending, ask]);

  if (!pending) return null;

  // Enter answers; Ctrl/Cmd+Enter (and Shift+Enter) insert a newline.
  // The browser doesn't insert a newline while a modifier is held, so
  // Ctrl+Enter splices one in manually at the caret.
  const handleTextareaKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key !== 'Enter') return;
    if (event.ctrlKey || event.metaKey) {
      event.preventDefault();
      const node = event.currentTarget;
      // setRangeText moves the DOM value + caret synchronously; syncing
      // state from node.value afterwards means the controlled re-render
      // is a no-op and the caret stays put (a deferred restore would
      // race with further keystrokes).
      node.setRangeText('\n', node.selectionStart, node.selectionEnd, 'end');
      setOtherText(node.value);
      return;
    }
    if (event.shiftKey) return; // native newline
    event.preventDefault();
    void submit();
  };

  // Enter anywhere else in the panel (e.g. on a radio option) also
  // answers. The textarea handles its own Enter above; preventDefault
  // keeps a focused Send button from double-submitting via native click.
  const handlePanelKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    if (event.key !== 'Enter' || event.ctrlKey || event.metaKey || event.shiftKey) return;
    if (event.target instanceof HTMLTextAreaElement) return;
    event.preventDefault();
    void submit();
  };

  return (
    <section
      ref={containerRef}
      className={styles.panel}
      role="dialog"
      aria-label="Agent question"
      tabIndex={-1}
      onKeyDown={handlePanelKeyDown}
    >
      <header className={styles.header}>
        <span className={styles.chip}>AGENT IS ASKING</span>
        <button
          type="button"
          className={styles.collapseBtn}
          onClick={() => setCollapsed((c) => !c)}
          aria-expanded={!collapsed}
          aria-label={collapsed ? 'Expand question' : 'Collapse question'}
          title={collapsed ? 'Expand' : 'Collapse'}
        >
          {collapsed ? '▸' : '▾'}
        </button>
      </header>

      {collapsed ? (
        <div className={styles.collapsedSummary}>{pending.question}</div>
      ) : (
        <div className={styles.body}>
      <div className={styles.question}>{pending.question}</div>

      {pending.extraContext && (
        <div className={styles.context}>{pending.extraContext}</div>
      )}

      {hasOptions ? (
        <div className={styles.options}>
          {isMultiSelect && (
            <span className={styles.multiHint}>Select all that apply.</span>
          )}
          {options.map((option, index) => (
            <label key={`opt-${index}`} className={styles.option}>
              <input
                type={isMultiSelect ? 'checkbox' : 'radio'}
                name={`ask-user-${pending.pendingId}`}
                value={index}
                checked={isMultiSelect ? selectedIndexes.has(index) : selectedIndex === index}
                onChange={() => {
                  if (isMultiSelect) {
                    setSelectedIndexes((current) => {
                      const next = new Set(current);
                      if (next.has(index)) next.delete(index);
                      else next.add(index);
                      return next;
                    });
                  } else {
                    setSelectedIndex(index);
                  }
                }}
                disabled={submitting}
              />
              <span className={styles.optionBody}>
                <span className={styles.optionLabel}>{option.label}</span>
                {option.description && (
                  <span className={styles.optionDescription}>{option.description}</span>
                )}
              </span>
            </label>
          ))}
          <label className={styles.option}>
            <input
              type={isMultiSelect ? 'checkbox' : 'radio'}
              name={`ask-user-${pending.pendingId}`}
              value={OTHER_INDEX}
              checked={isMultiSelect ? otherChecked : selectedIndex === OTHER_INDEX}
              onChange={() => {
                if (isMultiSelect) {
                  setOtherChecked((current) => !current);
                } else {
                  setSelectedIndex(OTHER_INDEX);
                }
              }}
              disabled={submitting}
            />
            <span className={styles.optionBody}>
              <span className={styles.optionLabel}>Other</span>
              <span className={styles.optionDescription}>
                {isMultiSelect ? 'Add a free-text answer.' : 'Type a free-text answer.'}
              </span>
            </span>
          </label>
          {(isMultiSelect ? otherChecked : selectedIndex === OTHER_INDEX) && (
            <textarea
              className={styles.textarea}
              value={otherText}
              onChange={(event) => setOtherText(event.target.value)}
              onKeyDown={handleTextareaKeyDown}
              placeholder="Type your answer… (Enter sends, Ctrl+Enter for a new line)"
              rows={3}
              disabled={submitting}
            />
          )}
        </div>
      ) : (
        <textarea
          className={styles.textarea}
          value={otherText}
          onChange={(event) => setOtherText(event.target.value)}
          onKeyDown={handleTextareaKeyDown}
          placeholder="Type your answer… (Enter sends, Ctrl+Enter for a new line)"
          rows={4}
          disabled={submitting}
        />
      )}

      {error && <div className={styles.error}>{error}</div>}

      <div className={styles.actions}>
        <button
          type="button"
          className={styles.submitButton}
          onClick={submit}
          disabled={!canSubmit}
        >
          {submitting ? 'Sending…' : 'Send answer'}
        </button>
      </div>
        </div>
      )}
    </section>
  );
};

export default AskUserPanel;
