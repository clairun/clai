import React, { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  listPendingPathGrantRequests,
  submitPathGrantDecision,
} from '../permissions/pathGrantsClient';
import type {
  FilesystemPathAccess,
  PathGrantDecision,
  PathGrantRequest,
} from '../generated/bindings';
import styles from './InlinePathGrantCard.module.css';

const PATH_GRANT_REQUEST_EVENT = 'path-grants://request';
// Backend cleared a pending grant without a user decision (run cancelled
// or ended, wait timed out, or a re-asked grant superseded it). Drop the
// stale card.
const PATH_GRANT_RESOLVED_EVENT = 'path-grants://resolved';

type PathAccess = FilesystemPathAccess;

interface PathGrantCardState {
  path: string;
  access: PathAccess;
}

interface InlinePathGrantCardProps {
  workspaceId: string | null;
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

const accessLabel = (access: PathAccess): string =>
  access === 'read_write' ? 'Read + write' : 'Read only';

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
const InlinePathGrantCard = ({ workspaceId }: InlinePathGrantCardProps) => {
  const [requests, setRequests] = useState<PathGrantRequest[]>([]);
  const [perCardState, setPerCardState] = useState<Record<string, PathGrantCardState>>({});
  const [submittingId, setSubmittingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
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
    setPerCardState({});
    setError(null);
    setSubmittingId(null);
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

    // Remove a card the backend cleared without a user decision (tool call
    // abandoned mid-wait, or timed out). requestId is globally unique, so no
    // workspace filter is needed.
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

  // Initialize per-card editable state when a new request arrives.
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- derive per-card editable state from the requests list: add entries for new requests, drop entries for resolved ones, and no-op when nothing changed. The functional updater + 'return current' early-out is the canonical React 18 derivation pattern; the [requests] dep is correct.
    setPerCardState((current) => {
      const next = { ...current };
      let changed = false;
      for (const req of requests) {
        if (next[req.requestId]) continue;
        next[req.requestId] = {
          path: req.requestedPath,
          access: req.requestedAccess,
        };
        changed = true;
      }
      for (const id of Object.keys(next)) {
        if (!requests.some((r) => r.requestId === id)) {
          delete next[id];
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, [requests]);

  // Auto-scroll on 0→1 transition only (don't yank the page on every new request).
  useEffect(() => {
    const prev = previousCountRef.current;
    const next = requests.length;
    previousCountRef.current = next;
    if (prev === 0 && next > 0 && firstCardRef.current) {
      firstCardRef.current.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }
  }, [requests.length]);

  const dismissRequest = useCallback((requestId: string) => {
    setRequests((current) => current.filter((q) => q.requestId !== requestId));
  }, []);

  const sendDecision = useCallback(
    async (requestId: string, decision: PathGrantDecision) => {
      if (submittingId) return;
      setSubmittingId(requestId);
      setError(null);
      try {
        await submitPathGrantDecision(requestId, decision);
        dismissRequest(requestId);
      } catch (e) {
        const message =
          typeof e === 'string' ? e : e instanceof Error ? e.message : 'Failed to submit decision';
        setError(message);
      } finally {
        setSubmittingId(null);
      }
    },
    [submittingId, dismissRequest],
  );

  const updateCardField = useCallback(
    <K extends keyof PathGrantCardState>(
      requestId: string,
      field: K,
      value: PathGrantCardState[K],
    ) => {
      setPerCardState((current) => ({
        ...current,
        [requestId]: { ...current[requestId], [field]: value } as PathGrantCardState,
      }));
    },
    [],
  );

  if (requests.length === 0) {
    return null;
  }

  return (
    <div className={styles.stack} aria-live="polite">
      {requests.map((req, cardIndex) => {
        const card = perCardState[req.requestId] || {
          path: req.requestedPath,
          access: req.requestedAccess,
        };
        const isSubmitting = submittingId === req.requestId;

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
            key={req.requestId}
            ref={cardIndex === 0 ? firstCardRef : null}
            className={styles.card}
            aria-label="Path grant requested"
          >
            <header className={styles.cardHeader}>
              <div className={styles.cardHeaderLeft}>
                <span className={styles.tag}>Path access requested</span>
                <span className={styles.cardSubtitle}>
                  {req.agentName
                    ? `Agent "${req.agentName}" wants to extend its filesystem grants:`
                    : 'An agent wants to extend its filesystem grants:'}
                </span>
              </div>
            </header>

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
                  onChange={(e) => updateCardField(req.requestId, 'path', e.target.value)}
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
                    onChange={() => updateCardField(req.requestId, 'access', 'read_only')}
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
                    onChange={() => updateCardField(req.requestId, 'access', 'read_write')}
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
                onClick={() =>
                  sendDecision(req.requestId, allowOnceDecision(card.path.trim(), card.access))
                }
                disabled={!canSubmit}
                title="Grant for the rest of this run only. Vanishes when the run ends."
              >
                Allow once
              </button>
              <button
                type="button"
                className={`${styles.btn} ${styles.btnAllowAlways}`}
                onClick={() =>
                  sendDecision(req.requestId, allowAlwaysDecision(card.path.trim(), card.access))
                }
                disabled={!canSubmit}
                title="Add to this agent's permanent grant list. Survives across runs."
              >
                Always allow (this agent)
              </button>
              <button
                type="button"
                className={`${styles.btn} ${styles.btnDeny}`}
                onClick={() => sendDecision(req.requestId, denyDecision())}
                disabled={isSubmitting}
              >
                Deny
              </button>
            </div>

            {error && submittingId === null && (
              <div className={styles.error}>{error}</div>
            )}
          </article>
        );
      })}
    </div>
  );
};

export default InlinePathGrantCard;
