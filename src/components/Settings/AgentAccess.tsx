/**
 * What an agent may do on this machine: path grants, network, shell, web
 * and the session bus. The overview shows it as one summary line; the full editor
 * is a drill-in view with paths and network on the left and the shell on the right.
 */

import React, { useId } from 'react';
import type {
  FilesystemPathGrant, GrantOrigin, NetworkMode, NetworkPolicy, SandboxSessionBusConfig, SandboxStatus, ShellAccessMode,
} from '../../generated/bindings';
import type { SandboxStatusState } from '../../hooks/useSandboxStatus';
import { unicodeHost } from '../../permissions/hostNames';
import { PathGrantEditor, PrefixListEditor, Switch } from './ListEditors';
import form from './SettingsForm.module.css';
import styles from './AgentBehaviorForm.module.css';

export interface AccessDraft {
  grants: FilesystemPathGrant[];
  shellMode: ShellAccessMode;
  askFirst: string[];
  web: boolean;
  /** The session bus switch: off is `deny`, on keeps `sessionBusMode`. */
  sessionBus: boolean;
  /** The saved bus mode; a three-way control can replace the switch. */
  sessionBusMode: SandboxSessionBusConfig;
  /** Not edited here yet; carried so a save keeps it. */
  allowedSockets: string[];
  /** Null only when the backend defaults could not be loaded; saving then lets the backend pick. */
  networkAccess: NetworkPolicy | null;
}

const SHELL_MODES: { mode: ShellAccessMode; label: string; hint: string }[] = [
  {
    mode: 'off',
    label: 'Off',
    hint: 'No shell. Agents use it for file operations, so the agent can neither read nor write files; charts and conversation history still work.',
  },
  {
    mode: 'restricted',
    label: 'Ask first',
    hint: 'Asks before commands on the ask-first list; everything else runs. It catches common accidents, not a determined agent.',
  },
  {
    mode: 'full',
    label: "Don't ask",
    hint: 'Runs every command without asking.',
  },
];

const NETWORK_MODES: { mode: NetworkMode; label: string; hint: string }[] = [
  {
    mode: 'off',
    label: 'Off',
    hint: 'No network for shell commands, web fetch or web search.',
  },
  {
    mode: 'allowlist',
    label: 'Allowlist',
    hint: 'Only the allowed hosts that are not denied. Web search still reaches its provider.',
  },
  {
    mode: 'host_network',
    label: 'Host network (unfiltered)',
    hint: 'Reaches any host, including services on this computer and your local network.',
  },
];

const INVALID_CHARACTERS = 'Use only letters, digits, hyphens and dots.';

// Without a sandbox the mode reaches only the in-app web tools.
const WEB_TOOLS_ONLY = 'web tools only';

/** The address in the form URLs print it, or null if it is not IPv6. */
const canonicalIpv6 = (text: string): string | null => {
  if (!/^[0-9a-f:.]+$/.test(text)) return null;
  try {
    return new URL(`http://[${text}]/`).hostname.slice(1, -1);
  } catch {
    return null;
  }
};

// A last label that makes URL parsers read the name as IPv4.
const endsInANumber = (label: string) => /^(\d+|0x[0-9a-f]*)$/.test(label);

const isIpv4 = (value: string) => {
  const parts = value.split('.');
  return parts.length === 4 && parts.every((part) => /^(0|[1-9]\d{0,2})$/.test(part) && Number(part) <= 255);
};

/** The name as URLs read it after IDNA (punycode, `。` as a dot), or null if they refuse it. */
const idnaHost = (value: string): string | null => {
  try {
    return new URL(`http://${value}/`).hostname;
  } catch {
    return null;
  }
};

/**
 * A host entry as the backend accepts it: a host name or IP, `*` only as a
 * whole label. Saving enforces the backend rule (`parse_entry`,
 * src-tauri/src/assistant/network/matcher.rs); this mirrors it for instant
 * feedback, and both run the cases in hostEntryCases.json. Returns the
 * entry to store, in the form the backend saves and compares (lower case,
 * punycode), or why the text is not one.
 */
export const parseHostEntry = (raw: string): string | { error: string } => {
  const value = raw.trim().toLowerCase().replace(/\.$/, '');
  if (!value) return { error: 'Enter a host name.' };
  if (/\s/.test(value)) return { error: 'A host name has no spaces.' };
  if (value.includes('://')) return { error: 'Enter the host only, without http:// or https://.' };
  if (value.includes('/')) return { error: 'Enter the host only, without a path.' };
  if (value.includes('@')) return { error: 'Enter the host only, without a user name.' };
  const bracketed = /^\[(.*)\]$/.exec(value)?.[1];
  const unbracketed = bracketed ?? value;
  if (unbracketed.includes(':')) {
    const address = canonicalIpv6(unbracketed);
    if (address) return address;
    return { error: bracketed === undefined ? 'Leave out the port: an entry matches every port.' : 'Not a valid IPv6 address.' };
  }
  // `idnaHost` builds a URL, where these would end the host.
  if (/[%?#\\]/.test(value)) return { error: INVALID_CHARACTERS };
  const labels = value.split('.');
  if (labels.some((label) => !label)) return { error: 'A host name has no empty labels.' };
  if (endsInANumber(labels.at(-1) ?? '')) {
    if (value.includes('*')) return { error: 'A wildcard cannot cover IP addresses.' };
    return isIpv4(value) ? value : { error: 'Not a valid IPv4 address; write it as four numbers, such as 10.0.0.5.' };
  }
  // The rest checks the name after IDNA, as the backend does: `*。com` is
  // *.com, and `１.２.３.４` is an IP address written in other digits.
  const host = idnaHost(value);
  const ascii = host?.split('.') ?? [];
  if (!host || endsInANumber(ascii.at(-1) ?? '')) return { error: INVALID_CHARACTERS };
  if (ascii.some((label) => !label)) return { error: 'A host name has no empty labels.' };
  if (ascii.some((label) => label.includes('*') && label !== '*')) {
    return { error: 'Use * only as a whole label, as in *.example.com.' };
  }
  if (ascii.every((label) => label === '*')) {
    return { error: 'It would allow every host; choose Host network instead.' };
  }
  if (!ascii.every((label) => label === '*' || /^[a-z0-9_-]+$/.test(label))) return { error: INVALID_CHARACTERS };
  // The backend form, so `github。com` and `Bücher.example` are found as
  // duplicates of the entries it saved.
  return host;
};

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
export const accessSummary = (draft: AccessDraft, sandbox: SandboxStatus | null = null): string[] => [
  `Shell: ${SHELL_MODES.find((m) => m.mode === draft.shellMode)?.label ?? draft.shellMode}`,
  draft.grants.length ? plural(draft.grants.length, 'path') : 'No paths',
  networkSummary(draft.networkAccess, sandbox),
  draft.web ? 'Web on' : 'Web off',
  busSummary(draft, sandbox),
];

/** Which bus the agent gets: only Full is the whole bus, and keyring only
 * where it cannot be filtered is no bus at all. */
const busSummary = (draft: AccessDraft, sandbox: SandboxStatus | null): string => {
  if (!draft.sessionBus) return 'Bus off';
  if (draft.sessionBusMode === 'full') return 'Bus: full';
  if (sandbox && !sandbox.sessionBusFilterAvailable) return 'Bus off (keyring only unavailable here)';
  return 'Bus: keyring only';
};

const networkSummary = (policy: NetworkPolicy | null, sandbox: SandboxStatus | null): string => {
  if (!policy) return 'Network: this PC’s default';
  const mode = policy.mode === 'allowlist'
    ? `Allowlist (${plural(policy.allowDomains.length, 'host')})`
    : policy.mode === 'off' ? 'Off' : 'Host network';
  // A stored Allowlist stops the shell there too, so it is not web tools only.
  const webToolsOnly = sandbox && !sandbox.available && policy.mode !== 'allowlist';
  return webToolsOnly ? `Network: ${mode} (${WEB_TOOLS_ONLY})` : `Network: ${mode}`;
};

/** The network mode picker and, in Allowlist, the two host lists. */
const NetworkSection = ({ policy, sandbox, onChange, disabled, headingId }: {
  policy: NetworkPolicy;
  sandbox: SandboxStatusState;
  onChange: (policy: NetworkPolicy) => void;
  disabled?: boolean;
  headingId: string;
}) => {
  const { status, failed, retry } = sandbox;
  // Unknown until the backend answers; nothing is offered before that.
  const known = !!status;
  const allowlistReason = status && !status.allowlistAvailable
    ? status.allowlistUnavailableReason ?? 'Not available here'
    : null;
  const hint = NETWORK_MODES.find((m) => m.mode === policy.mode)?.hint;
  const hintId = `${headingId}-hint`;
  const reasonId = `${headingId}-reason`;

  let statusHint: React.ReactNode;
  if (failed) {
    statusHint = (
      <p className={form.hintWarning} id={hintId} role="status">
        Could not check what this PC supports.{' '}
        <button type="button" className={form.linkButton} onClick={retry}>Retry</button>
      </p>
    );
  } else if (!status) {
    statusHint = <p className={form.hint} id={hintId} role="status">Checking what this PC supports…</p>;
  } else if (!status.available && policy.mode === 'allowlist') {
    // The Allowlist reason below says what happens to the shell.
    statusHint = null;
  } else if (!status.available) {
    statusHint = (
      <p className={form.hint} id={hintId}>
        On this PC it applies to {WEB_TOOLS_ONLY}; shell commands are not restricted.
      </p>
    );
  } else {
    statusHint = (
      <p className={policy.mode === 'host_network' ? form.hintWarning : form.hint} id={hintId}>{hint}</p>
    );
  }

  return (
    <>
      <h4 className={styles.subheading} id={headingId}>Network</h4>
      <div
        className={form.segmented}
        role="group"
        aria-labelledby={headingId}
        aria-describedby={statusHint ? hintId : reasonId}
      >
        {NETWORK_MODES.map(({ mode, label }) => {
          const unavailable = mode === 'allowlist' && !!allowlistReason;
          return (
            <button
              key={mode}
              type="button"
              className={form.segment}
              aria-pressed={policy.mode === mode}
              onClick={() => onChange({ ...policy, mode })}
              disabled={disabled || !known || unavailable}
              title={unavailable ? allowlistReason ?? undefined : undefined}
              aria-describedby={unavailable ? reasonId : undefined}
            >
              {label}
            </button>
          );
        })}
      </div>
      {statusHint}
      {allowlistReason && (
        policy.mode === 'allowlist' ? (
          <p className={form.hintWarning} id={reasonId}>
            Allowlist: {allowlistReason}. Shell commands will not run until you choose Off or Host network.
          </p>
        ) : (
          <p className={form.hint} id={reasonId}>Allowlist: {allowlistReason}.</p>
        )
      )}
      {known && policy.mode === 'allowlist' && (
        <div className={styles.prefixColumns}>
          <div className={styles.prefixList}>
            <span className={form.label}>Allowed hosts · {policy.allowDomains.length}</span>
            <p className={form.hint}>
              <code className={form.code}>*.example.com</code> matches its subdomains, not example.com itself.
            </p>
            <PrefixListEditor
              items={policy.allowDomains}
              onChange={(allowDomains) => onChange({ ...policy, allowDomains })}
              disabled={disabled}
              label="Allowed hosts"
              placeholder="example.com"
              parse={parseHostEntry}
              display={unicodeHost}
            />
          </div>
          <div className={styles.prefixList}>
            <span className={form.label}>Denied hosts · {policy.denyDomains.length}</span>
            <p className={form.hint}>Checked first, so they win over an allowed entry.</p>
            <PrefixListEditor
              items={policy.denyDomains}
              onChange={(denyDomains) => onChange({ ...policy, denyDomains })}
              disabled={disabled}
              label="Denied hosts"
              placeholder="uploads.example.com"
              parse={parseHostEntry}
              display={unicodeHost}
            />
          </div>
        </div>
      )}
    </>
  );
};

/** `sandbox` is the form's own lookup, so Retry here updates the overview too. */
export const AccessView = ({ draft, sandbox, onChange, disabled, headingRef, onBack, context }: {
  draft: AccessDraft;
  sandbox: SandboxStatusState;
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
          {draft.networkAccess ? (
            <NetworkSection
              policy={draft.networkAccess}
              sandbox={sandbox}
              onChange={(networkAccess) => onChange({ networkAccess })}
              disabled={disabled}
              headingId={`${headingId}-network`}
            />
          ) : (
            <>
              <h4 className={styles.subheading}>Network</h4>
              <p className={form.hint}>This PC’s default applies when you save.</p>
            </>
          )}
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

          {draft.shellMode === 'restricted' && (
            <div className={styles.prefixColumns}>
              <div className={styles.prefixList}>
                <span className={form.label}>Ask first</span>
                <p className={form.hint}>
                  A command that starts with one of these, anywhere in what the agent runs, waits for your
                  approval. Always allow on a card removes the rule here; add it back to be asked again.
                </p>
                <PrefixListEditor
                  items={draft.askFirst}
                  onChange={(askFirst) => onChange({ askFirst })}
                  disabled={disabled}
                  label="Ask-first command prefixes"
                  placeholder="git push"
                />
              </div>
            </div>
          )}

          <div className={styles.switches}>
            <Switch label="Web access" hint="Fetch pages and search the web." checked={draft.web} onChange={(web) => onChange({ web })} disabled={disabled} />
            <Switch
              label="Session D-Bus"
              hint={draft.sessionBusMode === 'full'
                ? 'Every desktop service on your session bus, unfiltered, including ones that can start programs outside the sandbox.'
                : 'Only your keyring (libsecret), so tools such as gh or git can use it; nothing else on your desktop.'}
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
