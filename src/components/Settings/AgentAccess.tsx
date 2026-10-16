/**
 * What an agent may do on this machine: path grants, shell, web and the
 * session bus. The overview shows it as one summary line; the full editor
 * is a drill-in view with paths on the left and the shell on the right.
 */

import React, { useId } from 'react';
import type { FilesystemPathGrant, GrantOrigin, ShellAccessMode } from '../../generated/bindings';
import { PathGrantEditor, PrefixListEditor, Switch } from './ListEditors';
import form from './SettingsForm.module.css';
import styles from './AgentBehaviorForm.module.css';

export interface AccessDraft {
  grants: FilesystemPathGrant[];
  shellMode: ShellAccessMode;
  allowed: string[];
  blocked: string[];
  web: boolean;
  sessionBus: boolean;
}

const SHELL_MODES: { mode: ShellAccessMode; label: string; hint: string }[] = [
  {
    mode: 'off',
    label: 'Off',
    hint: 'No shell. Agents use it for file operations, so the agent can neither read nor write files; charts and conversation history still work.',
  },
  {
    mode: 'restricted',
    label: 'Restricted',
    hint: 'A command outside the allowed list stops for your approval. An allowed list with nothing that writes leaves the agent able to read but not save.',
  },
  {
    mode: 'full',
    label: 'Full',
    hint: 'Any command runs without asking, except the blocked ones.',
  },
];

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;

export const grantOrigin = (grant: FilesystemPathGrant): { text: string; title?: string } | null => {
  const origin: GrantOrigin | null | undefined = grant.origin;
  if (!origin || origin.kind === 'manual') return null;
  if (origin.kind === 'credentialsPreset') return { text: 'Preset' };
  if (origin.kind === 'approval') {
    const when = origin.grantedAtUnixMs
      ? new Date(Number(origin.grantedAtUnixMs)).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
      : null;
    return { text: when ? `Approved ${when}` : 'Approved', title: origin.reason || undefined };
  }
  return null;
};

/** The overview's one-line answer to "what can it touch?". */
export const accessSummary = (draft: AccessDraft): string[] => [
  `Shell ${SHELL_MODES.find((m) => m.mode === draft.shellMode)?.label.toLowerCase() ?? draft.shellMode}`,
  draft.grants.length ? plural(draft.grants.length, 'path') : 'No paths',
  draft.web ? 'Web on' : 'Web off',
  draft.sessionBus ? 'Keyring on' : 'Keyring off',
];

export const AccessView = ({ draft, onChange, disabled, headingRef, onBack, context }: {
  draft: AccessDraft;
  onChange: (patch: Partial<AccessDraft>) => void;
  disabled?: boolean;
  headingRef: React.Ref<HTMLHeadingElement>;
  onBack: () => void;
  context: string;
}) => {
  const headingId = useId();
  const shellHint = SHELL_MODES.find((m) => m.mode === draft.shellMode)?.hint;

  return (
    <section className={`${form.panel} ${form.panelFill}`}>
      <header className={form.panelHeader}>
        <button type="button" className={form.quietButton} onClick={onBack} aria-label={`Back to ${context}`}>
          ‹ {context}
        </button>
        <h3 id={headingId} className={form.panelTitle} ref={headingRef} tabIndex={-1}>Local access</h3>
        <span className={form.panelMeta}>What this agent can do on your machine</span>
      </header>
      <div className={`${form.panelBody} ${styles.accessBody}`}>
        <div className={styles.accessColumn}>
          <h4 className={styles.subheading}>Paths</h4>
          <p className={form.hint}>
            Every new agent starts with <code className={form.code}>$HOME</code> read-only. Remove it to isolate the agent.
          </p>
          <PathGrantEditor
            grants={draft.grants}
            onChange={(grants) => onChange({ grants })}
            make={(path, access) => ({ path, access, origin: null })}
            describe={grantOrigin}
            disabled={disabled}
            label="Path grants"
          />
        </div>

        <div className={styles.accessColumn}>
          <h4 className={styles.subheading} id={`${headingId}-shell`}>Shell</h4>
          <div className={form.segmented} role="group" aria-labelledby={`${headingId}-shell`}>
            {SHELL_MODES.map(({ mode, label }) => (
              <button
                key={mode}
                type="button"
                className={form.segment}
                aria-pressed={draft.shellMode === mode}
                onClick={() => onChange({ shellMode: mode })}
                disabled={disabled}
              >
                {label}
              </button>
            ))}
          </div>
          <p className={form.hint}>{shellHint}</p>

          {draft.shellMode !== 'off' && (
            <div className={styles.prefixColumns}>
              {draft.shellMode === 'restricted' && (
                <div className={styles.prefixList}>
                  <span className={form.label}>Allowed without asking</span>
                  <PrefixListEditor
                    items={draft.allowed}
                    onChange={(allowed) => onChange({ allowed })}
                    disabled={disabled}
                    label="Allowed command prefixes"
                    placeholder="git status"
                  />
                </div>
              )}
              <div className={styles.prefixList}>
                <span className={form.label}>Always blocked</span>
                <PrefixListEditor
                  items={draft.blocked}
                  onChange={(blocked) => onChange({ blocked })}
                  disabled={disabled}
                  label="Blocked command prefixes"
                  placeholder="rm -rf"
                />
              </div>
            </div>
          )}

          <div className={styles.switches}>
            <Switch label="Web access" hint="Fetch pages and search the web." checked={draft.web} onChange={(web) => onChange({ web })} disabled={disabled} />
            <Switch
              label="Session D-Bus"
              hint="Lets tools such as gh or git use your keyring (libsecret)."
              checked={draft.sessionBus}
              onChange={(sessionBus) => onChange({ sessionBus })}
              disabled={disabled}
            />
          </div>
        </div>
      </div>
    </section>
  );
};
