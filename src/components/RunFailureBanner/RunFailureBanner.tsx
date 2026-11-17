import type { Ref } from 'react';
import type { RunFailure } from '../../assistant/runErrors';
import styles from './RunFailureBanner.module.css';

interface RunFailureBannerProps {
  failure: RunFailure;
  className?: string;
  ref?: Ref<HTMLDivElement>;
}

const RunFailureBanner = ({ failure, className, ref }: RunFailureBannerProps) => (
  <div
    ref={ref}
    className={className ? `${styles.banner} ${className}` : styles.banner}
    data-kind={failure.kind}
    role="alert"
  >
    <span className={styles.icon} aria-hidden="true">{failure.kind === 'usage_limit' ? '⏳' : '⚠'}</span>
    <div className={styles.body}>
      <span>{failure.text}</span>
      {failure.action && <span className={styles.action}>{failure.action}</span>}
    </div>
  </div>
);

export default RunFailureBanner;
