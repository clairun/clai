import { useEffect, useId, useRef, useState } from 'react';
import styles from './InlineApprovalCard.module.css';

// The backend accepts at most 500 characters and rejects the whole
// decision beyond that, so the field stops there.
const MAX_REASON_LENGTH = 500;
const limitReason = (value: string) => Array.from(value).slice(0, MAX_REASON_LENGTH).join('');

/**
 * The Deny button of a request card: it opens the reason panel instead of
 * denying, and closing the panel returns focus to it. Spread `denyButton`
 * on the button and pass `panelId` and `close` to the panel.
 */
export const useDenial = () => {
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const denyRef = useRef<HTMLButtonElement>(null);
  const close = () => {
    setOpen(false);
    denyRef.current?.focus();
  };
  return {
    open,
    close,
    panelId,
    denyButton: {
      ref: denyRef,
      onClick: () => (open ? close() : setOpen(true)),
      'aria-expanded': open,
      'aria-controls': open ? panelId : undefined,
    },
  };
};

interface DenyReasonPanelProps {
  id: string;
  /** The confirm button's label, such as “Deny command”. */
  confirmLabel: string;
  submitting: boolean;
  /** The reason, trimmed, or nothing when none was typed. */
  onConfirm: (reason?: string) => void;
  onClose: () => void;
}

/** The optional reason sent with a denial, and its confirm. Takes focus when it opens. */
export const DenyReasonPanel = ({ id, confirmLabel, submitting, onConfirm, onClose }: DenyReasonPanelProps) => {
  const [reason, setReason] = useState('');
  const reasonId = useId();
  const hintId = `${reasonId}-hint`;
  const countId = `${reasonId}-count`;
  const reasonRef = useRef<HTMLTextAreaElement>(null);
  // The backend counts code points, so the count does too.
  const reasonLength = Array.from(reason).length;

  useEffect(() => {
    reasonRef.current?.focus();
  }, []);

  return (
    <div
      id={id}
      className={styles.denialArea}
      onKeyDown={(e) => {
        if (e.key === 'Escape' && !submitting) {
          e.stopPropagation();
          onClose();
        }
      }}
    >
      <div className={styles.reasonField}>
        <label htmlFor={reasonId}>Reason for denying (optional)</label>
        <textarea
          ref={reasonRef}
          id={reasonId}
          value={reason}
          onChange={(e) => setReason(limitReason(e.target.value))}
          rows={2}
          disabled={submitting}
          placeholder="Tell the agent why"
          aria-describedby={`${hintId} ${countId}`}
        />
        <div className={styles.reasonMeta}>
          <span className={styles.mutedHint} id={hintId}>
            Sent to the agent with this response.
          </span>
          <span className={styles.mutedHint} id={countId}>
            {reasonLength}/{MAX_REASON_LENGTH} characters
          </span>
        </div>
      </div>
      <div className={styles.denialActions}>
        <button
          type="button"
          className={`${styles.btn} ${styles.btnConfirmDeny}`}
          onClick={() => onConfirm(reason.trim() || undefined)}
          disabled={submitting}
        >
          {submitting ? 'Submitting…' : confirmLabel}
        </button>
        <button type="button" className={styles.btn} onClick={onClose} disabled={submitting}>
          Cancel
        </button>
      </div>
    </div>
  );
};
