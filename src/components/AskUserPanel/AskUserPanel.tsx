import React, { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import useAssistantStore from '../../assistant/sessionStore';
import styles from './AskUserPanel.module.css';

const OTHER_INDEX = -1;

/**
 * Inline answer block for the `ask_user` tool.
 *
 * Renders when the active workspace session has a pending ask_user
 * request (the run is blocked waiting for the user). Mirrors the
 * AskUserQuestion UX in Claude Code itself: question text, optional
 * structured choices as radio buttons with an auto-added "Other"
 * free-text fallback, or a plain textarea when no options are provided.
 *
 * Submission round-trips via the `assistant_submit_user_input` Tauri
 * command, which delivers the answer back to the awaiting tool through
 * a oneshot channel keyed by pendingId. The run resumes in the same
 * MCP turn — no separate session, no follow-up run spawning.
 */
interface AskUserPanelProps {
  sessionId: string | null;
}

const AskUserPanel = ({ sessionId }: AskUserPanelProps) => {
  const pending = useAssistantStore((state) =>
    sessionId ? state.sessions[sessionId]?.pendingAskUser || null : null
  );

  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);
  const [otherText, setOtherText] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const containerRef = useRef<HTMLElement | null>(null);
  const previousPendingIdRef = useRef<string | null>(null);

  // Reset editable state when a new request arrives so a stale draft
  // from a previously-answered (or cancelled) question doesn't carry
  // over into the new prompt.
  useEffect(() => {
    if (pending?.pendingId === previousPendingIdRef.current) return;
    previousPendingIdRef.current = pending?.pendingId || null;
    setSelectedIndex(null);
    setOtherText('');
    setError('');
    setSubmitting(false);
  }, [pending?.pendingId]);

  // Focus the panel when a request appears so keyboard users land
  // directly on the prompt without having to tab through the chat.
  useEffect(() => {
    if (pending && containerRef.current) {
      containerRef.current.focus({ preventScroll: false });
    }
  }, [pending?.pendingId]);

  const options = useMemo(() => pending?.options || [], [pending?.options]);
  const hasOptions = options.length > 0;

  const canSubmit = useMemo(() => {
    if (!pending || submitting) return false;
    if (!hasOptions) return otherText.trim().length > 0;
    if (selectedIndex === null) return false;
    if (selectedIndex === OTHER_INDEX) return otherText.trim().length > 0;
    return true;
  }, [pending, submitting, hasOptions, selectedIndex, otherText]);

  if (!pending) return null;

  const submit = async () => {
    if (!canSubmit) return;
    setError('');
    setSubmitting(true);
    try {
      let answer: string;
      let selectedOptionIndex: number | null = null;
      if (!hasOptions || selectedIndex === OTHER_INDEX || selectedIndex === null) {
        answer = otherText.trim();
      } else {
        answer = options[selectedIndex]!.label;
        selectedOptionIndex = selectedIndex;
      }
      await invoke('assistant_submit_user_input', {
        request: {
          pendingId: pending.pendingId,
          answer,
          selectedOptionIndex,
        },
      });
      // The backend will emit `ask_user_resolved` which clears the
      // panel via the store; we don't optimistically clear here so the
      // panel stays visible if the backend rejects (e.g. the run was
      // already cancelled and the channel is gone).
    } catch (err) {
      const message =
        typeof err === 'string'
          ? err
          : err instanceof Error
            ? err.message
            : 'Failed to submit answer.';
      setError(message);
      setSubmitting(false);
    }
  };

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
      </header>

      <div className={styles.question}>{pending.question}</div>

      {pending.extraContext && (
        <div className={styles.context}>{pending.extraContext}</div>
      )}

      {hasOptions ? (
        <div className={styles.options}>
          {options.map((option, index) => (
            <label key={`opt-${index}`} className={styles.option}>
              <input
                type="radio"
                name={`ask-user-${pending.pendingId}`}
                value={index}
                checked={selectedIndex === index}
                onChange={() => setSelectedIndex(index)}
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
              type="radio"
              name={`ask-user-${pending.pendingId}`}
              value={OTHER_INDEX}
              checked={selectedIndex === OTHER_INDEX}
              onChange={() => setSelectedIndex(OTHER_INDEX)}
              disabled={submitting}
            />
            <span className={styles.optionBody}>
              <span className={styles.optionLabel}>Other</span>
              <span className={styles.optionDescription}>Type a free-text answer.</span>
            </span>
          </label>
          {selectedIndex === OTHER_INDEX && (
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
    </section>
  );
};

export default AskUserPanel;
