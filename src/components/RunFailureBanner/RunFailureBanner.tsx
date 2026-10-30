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
    data-limit={failure.isLimit || undefined}
    role="alert"
  >
    <span className={styles.icon} aria-hidden="true">{failure.isLimit ? '⏳' : '⚠'}</span>
    <span>{failure.text}</span>
  </div>
);

export default RunFailureBanner;
