import type { WorkspaceTaskResponse } from '../../generated/bindings';
import styles from './taskStatus.module.css';

/** Where a stopped or stopping task stands; nothing for any other status. */
export default function StopChip({ status }: { status: WorkspaceTaskResponse['status'] }) {
  if (status === 'stopping') {
    return <div className={styles.chip}>Stop requested · waiting for a safe point</div>;
  }
  if (status === 'stopped') {
    return <div className={styles.chip}>Stopped · send a message to resume</div>;
  }
  return null;
}
