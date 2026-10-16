import { useLayoutEffect, useRef, useState, type KeyboardEvent } from 'react';
import { computeTextareaSize } from '../../utils/composerTextarea';
import styles from './ChatComposer.module.css';

const LINE_HEIGHT = 19;
const MAX_HEIGHT = LINE_HEIGHT * 8;

interface ChatComposerProps {
  placeholder: string;
  ariaLabel: string;
  disabled?: boolean;
  /** Resolves with `error` to keep the text and show it; anything else clears the box. */
  onSend: (text: string) => Promise<{ error?: string }>;
}

/** Text-only message box: Enter sends, Shift+Enter adds a line. */
export default function ChatComposer({ placeholder, ariaLabel, disabled = false, onSend }: ChatComposerProps) {
  const [text, setText] = useState('');
  const [sending, setSending] = useState(false);
  const [error, setError] = useState('');
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useLayoutEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = `${LINE_HEIGHT}px`;
    const { height, overflowY } = computeTextareaSize(el.scrollHeight, LINE_HEIGHT, MAX_HEIGHT);
    el.style.height = `${height}px`;
    el.style.overflowY = overflowY;
  }, [text]);

  const canSend = !disabled && !sending && text.trim().length > 0;

  const send = async () => {
    if (!canSend) return;
    setSending(true);
    setError('');
    const result = await onSend(text.trim());
    setSending(false);
    if (result.error) {
      setError(result.error);
    } else {
      setText('');
    }
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault();
      void send();
    }
  };

  return (
    <div className={styles.composer}>
      {error && (
        <div role="alert" className={styles.error}>
          {error}
        </div>
      )}
      <div className={styles.row}>
        <textarea
          ref={textareaRef}
          className={styles.textarea}
          rows={1}
          value={text}
          placeholder={placeholder}
          aria-label={ariaLabel}
          disabled={disabled}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={handleKeyDown}
        />
        <button type="button" className={styles.sendButton} disabled={!canSend} onClick={() => void send()}>
          Send
        </button>
      </div>
    </div>
  );
}
