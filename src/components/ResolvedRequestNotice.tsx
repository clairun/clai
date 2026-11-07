import styles from './InlineApprovalCard.module.css';

/** Takes the place of a card's decisions once its request is no longer pending. */
export const ResolvedRequestNotice = ({ onDismiss }: { onDismiss?: () => void }) => (
  <div className={styles.segmentButtons}>
    <p className={styles.segmentHint} role="status">
      Already answered in another view, or its run has ended.
    </p>
    {onDismiss && (
      <button type="button" className={styles.btn} onClick={onDismiss}>
        Dismiss
      </button>
    )}
  </div>
);
