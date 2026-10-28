import React, { useEffect, useRef, useState, type ReactNode, type Ref } from 'react';
import { listen } from '@tauri-apps/api/event';
import { listPendingPathGrantRequests, submitPathGrantDecision } from '../permissions/pathGrantsClient';
import type {
  FilesystemPathAccess,
  PathGrantDecision,
  PathGrantRequest,
} from '../generated/bindings';
import { chipText } from './requestChips';
import styles from './InlinePathGrantCard.module.css';

const PATH_GRANT_REQUEST_EVENT = 'path-grants://request';
// A pending grant left the queue: a decision (here or in another view), a
// cancelled or ended run, or a re-asked grant superseded it. Drop the card.
const PATH_GRANT_RESOLVED_EVENT = 'path-grants://resolved';

type PathAccess = FilesystemPathAccess;

const denyDecision = (): PathGrantDecision => ({ kind: 'deny' });
const allowOnceDecision = (path: string, access: PathAccess): PathGrantDecision => ({
  kind: 'allowOnce',
  path,
  access,
});
const allowAlwaysDecision = (path: string, access: PathAccess): PathGrantDecision => ({
  kind: 'allowAlways',
  path,
  access,
  scope: 'agent',
});

interface PathGrantCardState {
  path: string;
  access: PathAccess;
}

// Paths that historically hold credentials or secrets. Surfacing this in
// the modal helps the user decide whether to approve a grant they didn't
// initiate themselves. We match these as either an exact path or a path
// starting with `<known>/`, anchored at the end of the user's HOME (so
// `/home/foo/.ssh` and `/Users/bar/.ssh` both trip the warning) AND
// also as suffix matches anywhere (so `/mnt/keys/.ssh` is still flagged
// if someone keeps SSH keys elsewhere).
const SENSITIVE_SUFFIXES = [
  '/.ssh',
  '/.gnupg',
  '/.password-store',
  '/.aws',
  '/.config/gcloud',
  '/.kube',
  '/.docker',
  '/.netrc',
  '/.npmrc',
  '/.pypirc',
];

// Paths that are likely too broad: they expose the user's entire identity
// or system. Anything that's an ancestor of (or equal to) these gets a
// distinct red warning, separate from "credentials" yellow.
const OVERLY_BROAD_PATHS = ['/', '/home', '/Users', '/etc', '/usr', '/var'];

const isSensitivePath = (path: string | null | undefined): boolean => {
  if (!path) return false;
  return SENSITIVE_SUFFIXES.some(
    (suffix) => path === suffix || path.endsWith(suffix) || path.includes(`${suffix}/`),
  );
};

const isOverlyBroadPath = (path: string | null | undefined): boolean => {
  if (!path) return false;
  const normalized = path.replace(/\/+$/, '') || '/';
  if (OVERLY_BROAD_PATHS.includes(normalized)) return true;
  const homeRootMatch = /^\/(home|Users)\/[^/]+$/.test(normalized);
  return homeRootMatch;
};


const accessLabel = (access: PathAccess): string =>
  access === 'read_write' ? 'Read + write' : 'Read only';

interface PathGrantCardProps {
  req: PathGrantRequest;
  /**
   * Draw the request as the agent saying it (the Fleet's asking face and
   * bubble): no tag or collapse, and short decision chips (full labels in
   * the tooltips) below. The warnings stay; narrowing the path is left to
   * the full card.
   */
  speaker?: (prompt: ReactNode) => ReactNode;
  /** The decision was accepted. */
  onDecided?: (requestId: string) => void;
  ref?: Ref<HTMLElement>;
}

/** One path-grant request and its decision. */
export const PathGrantCard = ({ req, speaker, onDecided, ref }: PathGrantCardProps) => {
  const [card, setCard] = useState<PathGrantCardState>({ path: req.requestedPath, access: req.requestedAccess });
  const [isSubmitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Collapse: squish the card to a one-line summary so a tall (or stacked)
  // request can't swallow the conversation. Default expanded.
  const [isCollapsed, setCollapsed] = useState(false);
  const spoken = Boolean(speaker);

  const sendDecision = async (decision: PathGrantDecision) => {
    setSubmitting(true);
    setError(null);
    try {
      await submitPathGrantDecision(req.requestId, decision);
      onDecided?.(req.requestId);
    } catch (e) {
      setError(typeof e === 'string' ? e : e instanceof Error ? e.message : 'Failed to submit decision');
    } finally {
      setSubmitting(false);
    }
  };

  // Validation: edited path must be the original or a descendant
  // (component-wise prefix). Edited access must be no stronger
  // than requested.
  const requestedPath = req.requestedPath;
  const isNarrower =
    card.path === requestedPath ||
    card.path.startsWith(`${requestedPath}${requestedPath.endsWith('/') ? '' : '/'}`);
  const accessIsAllowed =
    !(req.requestedAccess === 'read_only' && card.access === 'read_write');

  const pathInvalid = !card.path.trim() || !isNarrower;
  const accessInvalid = !accessIsAllowed;
  const canSubmit = !pathInvalid && !accessInvalid && !isSubmitting;

  const sensitive = isSensitivePath(card.path);
  const overlyBroad = isOverlyBroadPath(card.path);

  return (
    <article
      ref={ref}
      className={spoken ? `${styles.card} ${styles.spoken}` : styles.card}
      aria-label="Path grant requested"
    >
      {!spoken && (
        <header className={styles.cardHeader}>
          <div className={styles.cardHeaderLeft}>
            <span className={styles.tag}>Path access requested</span>
            <span className={styles.cardSubtitle}>
              {req.agentName
                ? `Agent "${req.agentName}" wants to extend its filesystem grants:`
                : 'An agent wants to extend its filesystem grants:'}
            </span>
          </div>
          <button
            type="button"
            className={styles.collapseBtn}
            onClick={() => setCollapsed((c) => !c)}
            aria-expanded={!isCollapsed}
            aria-label={isCollapsed ? 'Expand request' : 'Collapse request'}
            title={isCollapsed ? 'Expand' : 'Collapse'}
          >
            {isCollapsed ? '▸' : '▾'}
          </button>
        </header>
      )}

      {isCollapsed ? (
        <div className={styles.collapsedSummary}>{card.path}</div>
      ) : (
        <div className={styles.cardBody}>
          {speaker ? (
            speaker(
              <>
                May I {req.requestedAccess === 'read_write' ? 'read and write' : 'read'}{' '}
                <code className={styles.spokenPath}>{req.requestedPath}</code>?
                <span className={styles.spokenReason}>{req.reason || '(no reason given)'}</span>
              </>
            )
          ) : (
            <>
              <div className={styles.reasonBlock}>
                <span className={styles.reasonLabel}>Reason from agent:</span>
                <div className={styles.reasonText}>{req.reason || '(no reason given)'}</div>
              </div>

              <section className={styles.fields}>
                <label className={styles.fieldLabel}>
                  Path (you can narrow to a more specific subpath, but not widen)
                  <input
                    type="text"
                    className={`${styles.pathInput} ${pathInvalid ? styles.inputInvalid : ''}`}
                    value={card.path}
                    onChange={(e) => setCard({ ...card, path: e.target.value })}
                    disabled={isSubmitting}
                    spellCheck={false}
                    autoComplete="off"
                  />
                </label>
                {pathInvalid && card.path.trim() && (
                  <div className={styles.validationError}>
                    Path must be the requested path or a descendant of it. Requested:{' '}
                    <code>{requestedPath}</code>
                  </div>
                )}

                <div className={styles.accessRow}>
                  <span className={styles.fieldLabelInline}>Access:</span>
                  <label className={styles.radio}>
                    <input
                      type="radio"
                      name={`access-${req.requestId}`}
                      value="read_only"
                      checked={card.access === 'read_only'}
                      onChange={() => setCard({ ...card, access: 'read_only' })}
                      disabled={isSubmitting}
                    />
                    Read only
                  </label>
                  <label
                    className={`${styles.radio} ${
                      req.requestedAccess === 'read_only' ? styles.radioDisabled : ''
                    }`}
                    title={
                      req.requestedAccess === 'read_only'
                        ? "The agent asked for read-only; you can't upgrade it here."
                        : undefined
                    }
                  >
                    <input
                      type="radio"
                      name={`access-${req.requestId}`}
                      value="read_write"
                      checked={card.access === 'read_write'}
                      onChange={() => setCard({ ...card, access: 'read_write' })}
                      disabled={isSubmitting || req.requestedAccess === 'read_only'}
                    />
                    Read + write
                  </label>
                  <span className={styles.requestedHint}>
                    Agent requested:{' '}
                    <strong>{accessLabel(req.requestedAccess)}</strong>
                  </span>
                </div>
              </section>
            </>
          )}

          {(sensitive || overlyBroad) && (
            <div className={styles.warningStack}>
              {overlyBroad && (
                <div className={`${styles.warning} ${styles.warningCritical}`}>
                  ⚠ This is a very broad path. It exposes the user's entire account or
                  system to the agent. Consider narrowing to a specific subdirectory.
                </div>
              )}
              {sensitive && !overlyBroad && (
                <div className={`${styles.warning} ${styles.warningCaution}`}>
                  ⚠ This path typically holds credentials or secrets (e.g. SSH keys,
                  cloud tokens). With network enabled, anything readable here can be
                  exfiltrated by tools the agent runs. Grant carefully.
                </div>
              )}
            </div>
          )}

          <div className={styles.buttonRow}>
            <button
              type="button"
              className={`${styles.btn} ${styles.btnAllowOnce}`}
              onClick={() => sendDecision(allowOnceDecision(card.path.trim(), card.access))}
              disabled={!canSubmit}
              {...chipText(spoken, 'Allow once', 'Once', 'Grant for the rest of this run only. Vanishes when the run ends.')}
            />
            <button
              type="button"
              className={`${styles.btn} ${styles.btnAllowAlways}`}
              onClick={() => sendDecision(allowAlwaysDecision(card.path.trim(), card.access))}
              disabled={!canSubmit}
              {...chipText(
                spoken,
                'Always allow (this agent)',
                'Always',
                "Add to this agent's permanent grant list. Survives across runs."
              )}
            />
            <button
              type="button"
              className={`${styles.btn} ${styles.btnDeny}`}
              onClick={() => sendDecision(denyDecision())}
              disabled={isSubmitting}
              {...chipText(spoken, 'Deny', 'Deny')}
            />
          </div>

          {error && !isSubmitting && <div className={styles.error}>{error}</div>}
        </div>
      )}
    </article>
  );
};

/**
 * Inline path-grant approval card. Mirrors the structure of
 * InlineApprovalCard (command-prefix approvals) but for filesystem grants.
 *
 * Mount once per workspace conversation view; this component:
 *   - subscribes to `path-grants://request` events filtered by workspaceId,
 *   - seeds from the backend's pending-list on mount,
 *   - renders one card per pending request with editable path + access toggle,
 *   - enforces the narrowing invariants client-side (no widening, no
 *     upgrading RO→RW) so the user never gets a confusing backend rejection.
 */
const InlinePathGrantCard = ({ workspaceId }: { workspaceId: string | null }) => {
  const [requests, setRequests] = useState<PathGrantRequest[]>([]);
  const firstCardRef = useRef<HTMLElement | null>(null);
  const previousCountRef = useRef(0);

  useEffect(() => {
    // Switching workspaces reuses this component instance — the Workspace
    // page does NOT remount on workspace→workspace navigation — so drop any
    // requests still held from the previous workspace before seeding/
    // subscribing for the new one. Without this, workspace A's pending
    // path-grant card leaks into workspace B's view until A's request
    // happens to resolve.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- subscription+seed effect: clear stale per-workspace state on workspaceId change (component instance is reused across workspace navigation), then seed from pending list and subscribe to backend events. Effect is required for the listener registration + cleanup; the lint rule cannot model the seed/listen interaction.
    setRequests([]);
    previousCountRef.current = 0;

    if (!workspaceId) return undefined;

    let cancelled = false;

    listPendingPathGrantRequests(workspaceId)
      .then((pending) => {
        if (cancelled || !Array.isArray(pending) || pending.length === 0) return;
        setRequests((current) => {
          const known = new Set(current.map((q) => q.requestId));
          const additions = pending.filter((p) => !known.has(p.requestId));
          if (additions.length === 0) return current;
          return [...current, ...additions];
        });
      })
      .catch(() => {
        // Non-fatal — events still populate as new requests arrive.
      });

    const unlistenPromise = listen<PathGrantRequest>(PATH_GRANT_REQUEST_EVENT, (event) => {
      // The Tauri unlisten in the cleanup below is async — after a
      // workspace switch this stale listener can still fire before it
      // detaches, and its closure-captured workspaceId matches the OLD
      // workspace, so the workspace filter would pass and leak workspace
      // A's card into the (reused) component now showing workspace B.
      // The cancelled flag is the synchronous guard.
      if (cancelled) return;
      const req = event.payload;
      if (!req || !req.requestId || !req.requestedPath || !req.requestedAccess) return;
      if (req.workspaceId !== workspaceId) return;
      setRequests((current) => {
        if (current.some((q) => q.requestId === req.requestId)) return current;
        return [...current, req];
      });
    });

    // Remove the card once its grant leaves the queue. requestId is
    // globally unique, so no workspace filter is needed.
    const unlistenResolvedPromise = listen<{ requestId?: string }>(
      PATH_GRANT_RESOLVED_EVENT,
      (event) => {
        if (cancelled) return;
        const requestId = event.payload?.requestId;
        if (!requestId) return;
        setRequests((current) => current.filter((q) => q.requestId !== requestId));
      },
    );

    return () => {
      cancelled = true;
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
      unlistenResolvedPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [workspaceId]);

  // Auto-scroll on 0→1 transition only (don't yank the page on every new request).
  useEffect(() => {
    const prev = previousCountRef.current;
    const next = requests.length;
    previousCountRef.current = next;
    if (prev === 0 && next > 0 && firstCardRef.current) {
      firstCardRef.current.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }
  }, [requests.length]);

  if (requests.length === 0) {
    return null;
  }

  const dismissRequest = (requestId: string) =>
    setRequests((current) => current.filter((q) => q.requestId !== requestId));

  return (
    <div className={styles.stack} aria-live="polite">
      {requests.map((req, cardIndex) => (
        <PathGrantCard
          key={req.requestId}
          ref={cardIndex === 0 ? firstCardRef : null}
          req={req}
          onDecided={dismissRequest}
        />
      ))}
    </div>
  );
};

export default InlinePathGrantCard;
