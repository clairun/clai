/**
 * An agent's allowed ports on this computer, each bound to the program, or
 * the other user, that must serve it. An agent's request card saves them;
 * here they are listed and removed.
 */

import type { LocalPortGrant } from '../../generated/bindings';
import form from './SettingsForm.module.css';
import styles from './AgentBehaviorForm.module.css';

/** What must serve the port: as the row shows it, in its tooltip, and as its remove button names it. */
const servedBy = (grant: LocalPortGrant): { text: string; title?: string; name: string } => {
  if (grant.kind === 'program') {
    // The binary tells rows apart, and a long path cut short would hide it.
    const binary = grant.program.slice(grant.program.lastIndexOf('/') + 1);
    return { text: binary || grant.program, title: grant.program, name: grant.program };
  }
  // The name is display only, captured when the port was saved; the uid is matched.
  const text = grant.user ? `any program of user ${grant.user}` : `any program of uid ${grant.uid}`;
  return { text, title: grant.user ? `uid ${grant.uid}` : undefined, name: text };
};

export const LocalPortsEditor = ({
  grants,
  deniedBy,
  onChange,
  disabled,
}: {
  grants: LocalPortGrant[];
  /** A denied host entry that refuses every local port, such as `localhost`. */
  deniedBy?: string;
  onChange: (next: LocalPortGrant[]) => void;
  disabled?: boolean;
}) => (
  <div className={styles.prefixList}>
    <span className={form.label}>Local ports · {grants.length}</span>
    <p className={form.hint}>Ports on this computer, each allowed only while the program or user shown serves it.</p>
    {deniedBy && grants.length > 0 && (
      <p className={form.hintWarning}>Denied hosts include {deniedBy}, so these ports are refused.</p>
    )}
    {grants.length === 0 ? (
      <p className={form.listEmpty}>None. Approve an agent’s request to add one.</p>
    ) : (
      <ul className={form.list} aria-label="Local ports">
        {grants.map((grant, index) => {
          const target = `localhost:${grant.port}`;
          const server = servedBy(grant);
          // By position: a hand edit can repeat a grant, and saving drops the repeat.
          return (
            <li key={index} className={form.listItem}>
              <span className={form.mono}>{target}</span>
              <span
                className={`${form.listItemText} ${grant.kind === 'program' ? styles.localPortServer : styles.localPortUser}`}
                title={server.title}
              >
                {server.text}
              </span>
              <button
                type="button"
                className={form.removeButton}
                onClick={() => onChange(grants.filter((_, other) => other !== index))}
                disabled={disabled}
                aria-label={`Remove ${target} (${server.name})`}
              >
                ×
              </button>
            </li>
          );
        })}
      </ul>
    )}
  </div>
);
