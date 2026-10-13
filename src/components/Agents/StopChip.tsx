import type { WorkspaceTaskResponse } from '../../generated/bindings';
import styles from './taskStatus.module.css';

/** A stop that is waiting for a safe point; nothing for any other status. */
export default function StopChip({ status }: { status: WorkspaceTaskResponse['status'] }) {
  if (status !== 'stopping') return null;
  return <div className={styles.chip}>Stop requested · waiting for a safe point</div>;
}
