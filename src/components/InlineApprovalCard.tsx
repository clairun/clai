import React, { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  listPendingPermissionRequests,
  submitPermissionDecision,
} from '../permissions/client';
import type { PermissionRequest, SegmentDecision } from '../generated/bindings';
import styles from './InlineApprovalCard.module.css';

const PERMISSION_REQUEST_EVENT = 'permissions://request';

// Local UI alias matching the discriminator on SegmentDecision. Kept
// in its own type because the per-segment radio buttons need a flat
// enum, not the full discriminated union (the scope/prefix only enter
// at submit time).
type DecisionKind = 'allowOnce' | 'denyOnce' | 'allowAlways' | 'denyAlways';

interface SegmentCellState {
  prefix: string;
  decision: DecisionKind | null;
}

type CardState = Record<number, SegmentCellState>;

const allowOnce = (): SegmentDecision => ({ kind: 'allowOnce' });
const denyOnce = (): SegmentDecision => ({ kind: 'denyOnce' });
const allowAlways = (prefix: string): SegmentDecision => ({
  kind: 'allowAlways',
  scope: 'agent',
  prefix,
});
const denyAlways = (prefix: string): SegmentDecision => ({
  kind: 'denyAlways',
  scope: 'agent',
  prefix,
});

interface InlineApprovalCardProps {
  workspaceId: string | null;
}

/**
 * Inline permission-approval UI, rendered directly inside a workspace's
 * conversation view. Replaces the previous app-global modal: each
 * approval card now sits adjacent to the chat that produced it, so the
 * cause/effect is obvious and a pending request in workspace B no
 * longer hijacks the screen while the user is reading workspace A.
 *
 * Mount once per conversation render with the workspace's id. The
 * component subscribes to `permissions://request` events, filters by
 * workspaceId, and renders one card per pending request. A floating
 * chip near the input bar surfaces the count whenever there are
 * pending requests, with click-to-scroll into view.
 */
const InlineApprovalCard = ({ workspaceId }: InlineApprovalCardProps) => {
  const [requests, setRequests] = useState<PermissionRequest[]>([]);
  const [perCardState, setPerCardState] = useState<Record<string, CardState>>({});
  const [submittingId, setSubmittingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const firstCardRef = useRef<HTMLElement | null>(null);
  const previousCountRef = useRef(0);

  // Subscribe to backend approval-request events AND seed from the
  // backend's pending list on mount. The seed catches requests that
  // were registered before this component subscribed (e.g., the user
  // navigated here after the original event fired). The listener
  // catches new requests as they arrive. We dedupe by request_id so
  // the seed and an in-flight event for the same request don't
  // double-add.
  useEffect(() => {
    if (!workspaceId) return undefined;

    let cancelled = false;

    // Seed from existing pending requests for this workspace.
    listPendingPermissionRequests(workspaceId)
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
        // Non-fatal: events will still populate as requests arrive.
      });

    const unlistenPromise = listen<PermissionRequest>(PERMISSION_REQUEST_EVENT, (event) => {
      const req = event.payload;
      if (!req || !req.requestId || !Array.isArray(req.segments)) return;
      if (req.workspaceId !== workspaceId) return;
      setRequests((current) => {
        if (current.some((q) => q.requestId === req.requestId)) return current;
        return [...current, req];
      });
    });
    return () => {
      cancelled = true;
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [workspaceId]);

  // Initialize per-segment editable state for each new card.
  useEffect(() => {
    setPerCardState((current) => {
      const next = { ...current };
      let changed = false;
      for (const req of requests) {
        if (next[req.requestId]) continue;
        const initial: CardState = {};
        req.segments.forEach((seg, idx) => {
          initial[idx] = {
            prefix: seg.suggestedPrefix || '',
            decision: null,
          };
        });
        next[req.requestId] = initial;
        changed = true;
      }
      // Clean up stale state for requests that disappeared.
      for (const id of Object.keys(next)) {
        if (!requests.some((r) => r.requestId === id)) {
          delete next[id];
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, [requests]);

  // Auto-scroll into view on first arrival (0 → 1+ transition only —
  // we don't yank the page back every time a subsequent request lands).
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

  const sendDecisions = useCallback(
    async (requestId: string, decisions: SegmentDecision[]) => {
      if (submittingId) return;
      setSubmittingId(requestId);
      setError(null);
      try {
        await submitPermissionDecision(requestId, decisions);
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

  const setSegmentPrefix = useCallback((requestId: string, idx: number, prefix: string) => {
    setPerCardState((current) => ({
      ...current,
      [requestId]: {
        ...current[requestId],
        [idx]: { ...current[requestId]?.[idx], decision: current[requestId]?.[idx]?.decision ?? null, prefix },
      },
    }));
  }, []);

  const handleSegmentDecision = useCallback(
    (req: PermissionRequest, idx: number, kind: DecisionKind) => {
      if (submittingId) return;
      setPerCardState((current) => {
        const cardState: CardState = current[req.requestId] || {};
        const updatedCell: SegmentCellState = {
          prefix: cardState[idx]?.prefix ?? '',
          decision: kind,
        };
        const updatedCard: CardState = { ...cardState, [idx]: updatedCell };
        const updated = { ...current, [req.requestId]: updatedCard };

        const allDecided = req.segments.every((_, i) => updatedCard[i]?.decision);
        if (allDecided) {
          const decisions = req.segments.map((seg, i) => {
            const cell = updatedCard[i];
            const prefix = (cell.prefix || '').trim() || seg.text;
            switch (cell.decision) {
              case 'allowOnce':
                return allowOnce();
              case 'denyOnce':
                return denyOnce();
              case 'allowAlways':
                return allowAlways(prefix);
              case 'denyAlways':
                return denyAlways(prefix);
              default:
                return denyOnce();
            }
          });
          sendDecisions(req.requestId, decisions);
        }
        return updated;
      });
    },
    [submittingId, sendDecisions],
  );

  const denyAll = useCallback(
    (req: PermissionRequest) => {
      const decisions = req.segments.map(() => denyOnce());
      sendDecisions(req.requestId, decisions);
    },
    [sendDecisions],
  );

  if (requests.length === 0) {
    return null;
  }

  return (
    <div className={styles.stack} aria-live="polite">
        {requests.map((req, cardIndex) => {
          const cardState = perCardState[req.requestId] || {};
          const isSubmitting = submittingId === req.requestId;
          return (
            <article
              key={req.requestId}
              ref={cardIndex === 0 ? firstCardRef : null}
              className={styles.card}
              aria-label="Permission requested"
            >
              <header className={styles.cardHeader}>
                <div className={styles.cardHeaderLeft}>
                  <span className={styles.tag}>Permission requested</span>
                  <span className={styles.cardSubtitle}>
                    {req.agentName
                      ? `Agent "${req.agentName}" wants to run:`
                      : 'An agent wants to run:'}
                  </span>
                </div>
              </header>
              <pre className={styles.command}>{req.command}</pre>
              <section className={styles.segments}>
                <p className={styles.segmentsLabel}>
                  {req.segments.length === 1
                    ? 'Choose how to handle this command:'
                    : `${req.segments.length} parts need a decision (submits as soon as every part is decided):`}
                </p>
                {req.segments.map((seg, idx) => {
                  const cell = cardState[idx] || {};
                  const isOpaque = seg.kind === 'opaque';
                  return (
                    <div key={idx} className={styles.segmentRow}>
                      {req.segments.length > 1 && (
                        <div className={styles.segmentText}>
                          <code>{seg.text}</code>
                          {isOpaque && (
                            <span
                              className={styles.opaqueTag}
                              title="Contains substitution, executor, redirect, or control-flow — can't be safely allowlisted."
                            >
                              opaque
                            </span>
                          )}
                        </div>
                      )}
                      {!isOpaque && (
                        <label className={styles.prefixField}>
                          Save as prefix:
                          <input
                            type="text"
                            value={cell.prefix || ''}
                            onChange={(e) => setSegmentPrefix(req.requestId, idx, e.target.value)}
                            disabled={isSubmitting}
                            spellCheck={false}
                            autoComplete="off"
                          />
                        </label>
                      )}
                      <div className={styles.segmentButtons}>
                        <button
                          type="button"
                          className={`${styles.btn} ${styles.btnAllowOnce} ${cell.decision === 'allowOnce' ? styles.btnSelected : ''}`}
                          onClick={() => handleSegmentDecision(req, idx, 'allowOnce')}
                          disabled={isSubmitting}
                        >
                          Allow once
                        </button>
                        {!isOpaque && (
                          <button
                            type="button"
                            className={`${styles.btn} ${styles.btnAllow} ${cell.decision === 'allowAlways' ? styles.btnSelected : ''}`}
                            onClick={() => handleSegmentDecision(req, idx, 'allowAlways')}
                            disabled={isSubmitting}
                            title="Save the prefix to .clai/permissions.json for this agent"
                          >
                            Always allow (this agent)
                          </button>
                        )}
                        <button
                          type="button"
                          className={`${styles.btn} ${styles.btnDeny} ${cell.decision === 'denyAlways' ? styles.btnSelected : ''}`}
                          onClick={() => handleSegmentDecision(req, idx, 'denyAlways')}
                          disabled={isSubmitting}
                          title="Block the prefix in .clai/permissions.json for this agent"
                        >
                          Always deny (this agent)
                        </button>
                        <button
                          type="button"
                          className={`${styles.btn} ${styles.btnDenyOnce} ${cell.decision === 'denyOnce' ? styles.btnSelected : ''}`}
                          onClick={() => handleSegmentDecision(req, idx, 'denyOnce')}
                          disabled={isSubmitting}
                        >
                          Deny once
                        </button>
                      </div>
                    </div>
                  );
                })}
              </section>
              {error && submittingId === null && (
                <div className={styles.error}>{error}</div>
              )}
              {req.segments.length > 1 && (
                <footer className={styles.cardFooter}>
                  <button
                    type="button"
                    className={`${styles.btn} ${styles.btnDenyAll}`}
                    onClick={() => denyAll(req)}
                    disabled={isSubmitting}
                    title="Deny every part of this pipeline in one click"
                  >
                    Deny entire command
                  </button>
                </footer>
              )}
            </article>
          );
        })}
    </div>
  );
};

export default InlineApprovalCard;
