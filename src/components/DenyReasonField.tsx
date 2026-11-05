import { useId, useState } from 'react';
import styles from './InlineApprovalCard.module.css';

// The backend accepts at most 500 characters and rejects the whole
// decision beyond that, so the field stops there.
const MAX_REASON_LENGTH = 500;
const limitReason = (value: string) => Array.from(value).slice(0, MAX_REASON_LENGTH).join('');
const labelIdOf = (id: string) => `${id}-label`;

/**
 * The reason a request card's Deny sends. Spread `field` on the
 * DenyReasonField, point the Deny button's aria-describedby at
 * `labelId` so it says a reason can go with it, and send `decision()`.
 */
export const useDenyReason = () => {
  const [value, setValue] = useState('');
  const id = useId();
  return {
    field: { id, value, onChange: setValue },
    labelId: labelIdOf(id),
    /** A denial with the reason trimmed, or with none when none was typed. */
    decision: (): { kind: 'deny'; reason?: string } => {
      const reason = value.trim();
      return reason ? { kind: 'deny', reason } : { kind: 'deny' };
    },
  };
};

interface DenyReasonFieldProps {
  id: string;
  value: string;
  onChange: (value: string) => void;
  disabled: boolean;
}

/**
 * The optional reason for a denial, shown with the card so Deny answers in
 * one click. It never takes focus by itself: cards arrive while the user may
 * be typing elsewhere. Enter does nothing, so a typed reason is only sent by
 * Deny.
 */
export const DenyReasonField = ({ id, value, onChange, disabled }: DenyReasonFieldProps) => {
  const hintId = `${id}-hint`;
  const countId = `${id}-count`;
  // The backend counts code points, so the count does too.
  const length = Array.from(value).length;
  return (
    <div className={styles.reasonField}>
      <label htmlFor={id} id={labelIdOf(id)}>Reason for denying (optional)</label>
      <input
        type="text"
        id={id}
        value={value}
        onChange={(e) => onChange(limitReason(e.target.value))}
        disabled={disabled}
        placeholder="Tell the agent why"
        autoComplete="off"
        aria-describedby={`${hintId} ${countId}`}
      />
      <div className={styles.reasonMeta}>
        <span className={styles.mutedHint} id={hintId}>
          Sent to the agent only if you deny.
        </span>
        <span className={styles.mutedHint} id={countId}>
          {length}/{MAX_REASON_LENGTH} characters
        </span>
      </div>
    </div>
  );
};
