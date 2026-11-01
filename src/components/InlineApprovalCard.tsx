import React, { useEffect, useId, useRef, useState, type ReactNode, type Ref } from 'react';
import { listen } from '@tauri-apps/api/event';
import { listPendingPermissionRequests, submitPermissionDecision } from '../permissions/client';
import type { PermissionRequest, SegmentDecision } from '../generated/bindings';
import { chipText } from './requestChips';
import styles from './InlineApprovalCard.module.css';

const PERMISSION_REQUEST_EVENT = 'permissions://request';
// A pending request left the queue: a decision (here or in another view), a
// cancelled or ended run, or a re-asked command superseded it. Drop the
// card. Payload: { requestId }.
const PERMISSION_RESOLVED_EVENT = 'permissions://resolved';

// The backend accepts at most 500 characters and rejects the whole
// decision beyond that, so the field stops there.
const MAX_REASON_LENGTH = 500;
const limitReason = (value: string) => Array.from(value).slice(0, MAX_REASON_LENGTH).join('');

const allowOnce = (): SegmentDecision => ({ kind: 'allowOnce' });
const denyOnce = (reason?: string): SegmentDecision => ({ kind: 'denyOnce', ...(reason ? { reason } : {}) });
const allowAlways = (prefix: string): SegmentDecision => ({ kind: 'allowAlways', scope: 'agent', prefix });
const denyAlways = (prefix: string, reason?: string): SegmentDecision => ({ kind: 'denyAlways', scope: 'agent', prefix, ...(reason ? { reason } : {}) });

/** A spoken chip names its prefix by its first words; its tooltip has it whole. */
const CHIP_PREFIX_CHARS = 16;
const chipPrefix = (prefix: string): string => {
  const line = prefix.split('\n', 1)[0]!.trim();
  return line.length > CHIP_PREFIX_CHARS || line !== prefix.trim() ? `${line.slice(0, CHIP_PREFIX_CHARS - 1)}…` : line;
};

// Local UI alias matching the discriminator on SegmentDecision. Kept
// in its own type because the per-segment radio buttons need a flat
// enum, not the full discriminated union (the scope/prefix only enter
// at submit time).
type DecisionKind = 'allowOnce' | 'denyOnce' | 'allowAlways' | 'denyAlways';

const isDenial = (decision: DecisionKind | null): boolean =>
  decision === 'denyOnce' || decision === 'denyAlways';

interface SegmentCellState {
  prefix: string;
  decision: DecisionKind | null;
}

type CardState = Record<number, SegmentCellState>;

interface ApprovalCardProps {
  req: PermissionRequest;
  /**
   * Draw the command as the agent saying it (the Fleet's asking face and
   * bubble): no tag or collapse, each part on one line, and short decision
   * chips (full labels in the tooltips) below. No prefix editing: an
   * "always" chip saves the suggested prefix and shows it (`Always · cargo`).
   */
  speaker?: (prompt: ReactNode) => ReactNode;
  /** The decision was accepted. */
  onDecided?: (requestId: string) => void;
  ref?: Ref<HTMLElement>;
}

/** One permission request and its decisions. */
export const ApprovalCard = ({ req, speaker, onDecided, ref }: ApprovalCardProps) => {
  const [cells, setCells] = useState<CardState>(() =>
    Object.fromEntries(req.segments.map((seg, idx) => [idx, { prefix: seg.suggestedPrefix || '', decision: null }]))
  );
  const [isSubmitting, setSubmitting] = useState(false);
  // Latched once the decision is sent, so it cannot be sent twice.
  const sent = useRef(false);
  const [error, setError] = useState<string | null>(null);
  // One reason for the whole request, sent with the denial response.
  const [reason, setReason] = useState('');
  const reasonId = useId();
  const reasonHintId = `${reasonId}-hint`;
  const reasonCountId = `${reasonId}-count`;
  // The backend counts code points, so the count does too.
  const reasonLength = Array.from(reason).length;
  // Collapse: squish the card to a one-line summary so a tall (or stacked)
  // request can't swallow the conversation. Default expanded.
  const [isCollapsed, setCollapsed] = useState(false);
  const spoken = Boolean(speaker);

  const sendDecisions = async (decisions: SegmentDecision[]) => {
    // One decision per request: a click that lands after the send must not
    // send the decision again.
    if (sent.current) return;
    sent.current = true;
    setSubmitting(true);
    setError(null);
    try {
      await submitPermissionDecision(req.requestId, decisions);
      onDecided?.(req.requestId);
    } catch (e) {
      // A decision that failed can be sent again.
      sent.current = false;
      setError(typeof e === 'string' ? e : e instanceof Error ? e.message : 'Failed to submit decision');
    } finally {
      setSubmitting(false);
    }
  };

  const setSegmentPrefix = (idx: number, prefix: string) =>
    setCells((current) => ({ ...current, [idx]: { decision: current[idx]?.decision ?? null, prefix } }));

  // The reason rides along with the denial it belongs to; an allow never
  // carries one.
  const decisionsFrom = (selected: CardState, why: string): SegmentDecision[] => {
    const trimmedReason = why.trim() || undefined;
    return req.segments.map((seg, i) => {
      const cell = selected[i]!; // every segment is decided
      const prefix = (cell.prefix || '').trim() || seg.text;
      switch (cell.decision) {
        case 'allowOnce':
          return allowOnce();
        case 'allowAlways':
          return allowAlways(prefix);
        case 'denyAlways':
          return denyAlways(prefix, trimmedReason);
        default:
          return denyOnce(trimmedReason);
      }
    });
  };

  const handleSegmentDecision = (idx: number, kind: DecisionKind) => {
    const next: CardState = { ...cells, [idx]: { prefix: cells[idx]?.prefix ?? '', decision: kind } };
    setCells(next);
    if (!req.segments.every((_, i) => next[i]?.decision)) return;
    // A denial waits for the confirm button below: the reason is typed
    // there, and only the denial carries it.
    if (req.segments.some((_, i) => isDenial(next[i]?.decision ?? null))) return;
    sendDecisions(decisionsFrom(next, reason));
  };

  // Deny every part; the one reason field applies to all of them.
  const denyAll = () =>
    setCells((current) => {
      const next: CardState = {};
      req.segments.forEach((_, idx) => {
        next[idx] = { prefix: current[idx]?.prefix ?? '', decision: 'denyOnce' };
      });
      return next;
    });

  const confirmDenial = () => sendDecisions(decisionsFrom(cells, reason));

  const hasDenial = req.segments.some((_, i) => isDenial(cells[i]?.decision ?? null));
  const allDecided = req.segments.every((_, i) => cells[i]?.decision);
  // "Deny always" writes a block rule that lasts, so the confirm says so.
  const blocksAlways = req.segments.some((_, i) => cells[i]?.decision === 'denyAlways');
  const confirmLabel = (): string => {
    if (isSubmitting) return 'Submitting…';
    if (req.segments.length > 1) return 'Submit decisions';
    return blocksAlways ? 'Deny and block always' : 'Deny command';
  };

  // Batch "trust this whole command" shortcut. For each segment that
  // has a suggestedPrefix (either Simple or Opaque whose head is
  // recognized), submits AllowAlways with the user's edited prefix or
  // the suggestion; segments without a stable prefix fall back to
  // AllowOnce so the run still proceeds. Mirrors the per-row
  // "Always allow" buttons collapsed into a single click for multi-
  // segment commands like `cd && go run foo` or `which … || find …`.
  const allowAllAlways = () =>
    sendDecisions(
      req.segments.map((seg, idx) => {
        const prefix = (cells[idx]?.prefix ?? seg.suggestedPrefix ?? '').trim();
        return prefix ? allowAlways(prefix) : allowOnce();
      })
    );

  // A shared teammate keeps its command decisions on its library
  // definition, so "always" applies wherever that agent works — here,
  // in the workspaces listed, and in any that add it later.
  const isSharedAgent = req.persistsToSharedAgent;
  const sharedReach = req.alsoAffectsWorkspaces || [];
  const savedOn = isSharedAgent
    ? 'Saved on the shared agent, so it applies wherever this agent works'
    : 'Saved on this workspace\'s own agent';
  const reach = isSharedAgent ? 'every workspace' : 'this agent';
  // An "always" decision: the spoken chip names the prefix it saves.
  const alwaysChip = (verb: 'allow' | 'deny', prefix: string) =>
    spoken
      ? {
          title: `Always ${verb} commands starting with \`${prefix}\` (${reach})`,
          children: (
            <>
              {verb === 'allow' ? 'Always' : 'Never'} · <code>{chipPrefix(prefix)}</code>
            </>
          ),
        }
      : { title: savedOn, children: `Always ${verb} (${reach})` };

  return (
    <article
      ref={ref}
      className={spoken ? `${styles.card} ${styles.spoken}` : styles.card}
      aria-label="Permission requested"
    >
      {!spoken && (
        <header className={styles.cardHeader}>
          <div className={styles.cardHeaderLeft}>
            <span className={styles.tag}>Permission requested</span>
            <span className={styles.cardSubtitle}>
              {req.agentName ? `Agent "${req.agentName}" wants to run:` : 'An agent wants to run:'}
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
        <pre className={`${styles.command} ${styles.collapsedSummary}`}>{req.command}</pre>
      ) : (
        <div className={styles.cardBody}>
          {/* Spoken too: the parts leave out separators and parts that need no decision. */}
          {speaker ? (
            speaker(
              <>
                May I run this?
                <pre className={`${styles.command} ${styles.spokenCommand}`} tabIndex={0} role="region" aria-label="Command">
                  {req.command}
                </pre>
              </>
            )
          ) : (
            <pre className={styles.command}>{req.command}</pre>
          )}
          <section className={styles.segments}>
            {isSharedAgent && !spoken && (
              <p className={styles.segmentsLabel}>
                {sharedReach.length > 0
                  ? `An "always" decision is saved on this shared agent, so it also applies to it in: ${sharedReach.join(', ')}.`
                  : 'An "always" decision is saved on this shared agent, so it applies wherever this agent is added.'}
              </p>
            )}
            {!spoken && (
              <p className={styles.segmentsLabel}>
                {req.segments.length === 1
                  ? 'Choose how to handle this command (a denial needs a confirm):'
                  : `${req.segments.length} parts need a decision (confirm after choosing any denial):`}
              </p>
            )}
            {req.segments.map((seg, idx) => {
              const cell: SegmentCellState = cells[idx] || { prefix: '', decision: null };
              const isOpaque = seg.kind === 'opaque';
              // The backend now fills `suggestedPrefix` for Opaque
              // segments too (the binary head IS a stable identifier
              // even when the surrounding syntax is opaque). The
              // persistence affordances key off the prefix being
              // non-empty rather than the kind tag, so any Opaque
              // segment whose head was recognized gets the same
              // "Always allow" workflow as Simple ones.
              const canPersist = Boolean(seg.suggestedPrefix);
              return (
                <div key={idx} className={styles.segmentRow}>
                  {(req.segments.length > 1 || isOpaque) && (
                    <div className={styles.segmentText}>
                      {req.segments.length > 1 && <code title={spoken ? seg.text : undefined}>{seg.text}</code>}
                      {isOpaque && (
                        <span
                          className={styles.opaqueTag}
                          title="Uses a substitution, redirect, pipe-to-shell or control flow, so what it does isn't just the command it starts. Read it carefully before allowing."
                        >
                          complex
                        </span>
                      )}
                    </div>
                  )}
                  {canPersist && !spoken && (
                    <label className={styles.prefixField}>
                      Save as prefix:
                      <input
                        type="text"
                        value={cell.prefix || ''}
                        onChange={(e) => setSegmentPrefix(idx, e.target.value)}
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
                      onClick={() => handleSegmentDecision(idx, 'allowOnce')}
                      disabled={isSubmitting}
                      {...chipText(spoken, 'Allow once', 'Once')}
                    />
                    {canPersist && (
                      <button
                        type="button"
                        className={`${styles.btn} ${styles.btnAllow} ${cell.decision === 'allowAlways' ? styles.btnSelected : ''}`}
                        onClick={() => handleSegmentDecision(idx, 'allowAlways')}
                        disabled={isSubmitting}
                        {...alwaysChip('allow', seg.suggestedPrefix)}
                      />
                    )}
                    {(canPersist || !spoken) && (
                      <button
                        type="button"
                        className={`${styles.btn} ${styles.btnDeny} ${cell.decision === 'denyAlways' ? styles.btnSelected : ''}`}
                        onClick={() => handleSegmentDecision(idx, 'denyAlways')}
                        disabled={isSubmitting}
                        {...alwaysChip('deny', seg.suggestedPrefix)}
                      />
                    )}
                    <button
                      type="button"
                      className={`${styles.btn} ${styles.btnDenyOnce} ${cell.decision === 'denyOnce' ? styles.btnSelected : ''}`}
                      onClick={() => handleSegmentDecision(idx, 'denyOnce')}
                      disabled={isSubmitting}
                      {...chipText(spoken, 'Deny once', 'Deny')}
                    />
                  </div>
                </div>
              );
            })}
          </section>
          {hasDenial && (
            <div className={styles.denialArea}>
              <div className={styles.reasonField}>
                <label htmlFor={reasonId}>Reason for denying (optional)</label>
                <textarea
                  id={reasonId}
                  value={reason}
                  onChange={(e) => setReason(limitReason(e.target.value))}
                  rows={2}
                  disabled={isSubmitting}
                  placeholder="Tell the agent why"
                  aria-describedby={`${reasonHintId} ${reasonCountId}`}
                />
                <div className={styles.reasonMeta}>
                  <span className={styles.mutedHint} id={reasonHintId}>
                    Sent to the agent with this response.
                  </span>
                  <span className={styles.mutedHint} id={reasonCountId}>
                    {reasonLength}/{MAX_REASON_LENGTH} characters
                  </span>
                </div>
              </div>
              <div className={styles.denialActions}>
                <button
                  type="button"
                  className={`${styles.btn} ${styles.btnConfirmDeny}`}
                  onClick={confirmDenial}
                  disabled={isSubmitting || !allDecided}
                >
                  {confirmLabel()}
                </button>
                {!allDecided && <span className={styles.mutedHint}>Choose for every part to continue</span>}
              </div>
            </div>
          )}
          {error && !isSubmitting && <div className={styles.error}>{error}</div>}
          {req.segments.length > 1 && (
            <footer className={styles.cardFooter}>
              {(!spoken || req.segments.some((seg) => seg.suggestedPrefix)) && (
                <button
                  type="button"
                  className={`${styles.btn} ${styles.btnAllow}`}
                  onClick={allowAllAlways}
                  disabled={isSubmitting}
                  {...chipText(
                    spoken,
                    'Allow all (always)',
                    'All always',
                    'Persist every persistable prefix and run. Segments without a stable prefix fall back to allow-once.'
                  )}
                />
              )}
              <button
                type="button"
                className={`${styles.btn} ${styles.btnDenyAll}`}
                onClick={denyAll}
                disabled={isSubmitting}
                {...chipText(spoken, 'Deny entire command', 'Deny all', 'Deny every part of this pipeline')}
              />
            </footer>
          )}
        </div>
      )}
    </article>
  );
};

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
const InlineApprovalCard = ({ workspaceId }: { workspaceId: string | null }) => {
  const [requests, setRequests] = useState<PermissionRequest[]>([]);
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
    // Switching workspaces reuses this component instance — the Workspace
    // page does NOT remount on workspace→workspace navigation — so any
    // requests still held from the previous workspace must be dropped
    // before we seed/subscribe for the new one. Without this, workspace
    // A's pending approval card leaks into workspace B's view (it lingers
    // until A's request happens to resolve). Clearing here keys the card's
    // per-workspace state to workspaceId.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- subscription+seed effect: clear stale per-workspace state on workspaceId change (component instance is reused across workspace navigation), then seed from pending list and subscribe to backend events. Effect is required for the listener registration + cleanup; the lint rule cannot model the seed/listen interaction.
    setRequests([]);
    previousCountRef.current = 0;

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
      // The Tauri unlisten in the cleanup below is async — after a
      // workspace switch this stale listener can still fire before it
      // detaches, and its closure-captured workspaceId matches the OLD
      // workspace, so the `req.workspaceId !== workspaceId` filter would
      // pass and leak workspace A's card into the (reused) component now
      // showing workspace B. The cancelled flag is the synchronous guard.
      if (cancelled) return;
      const req = event.payload;
      if (!req || !req.requestId || !Array.isArray(req.segments)) return;
      if (req.workspaceId !== workspaceId) return;
      setRequests((current) => {
        if (current.some((q) => q.requestId === req.requestId)) return current;
        return [...current, req];
      });
    });

    // Remove the card once its request leaves the queue. requestId
    // is globally unique, so no workspace filter is needed — if it isn't in
    // this card's list, the filter is a no-op.
    const unlistenResolvedPromise = listen<{ requestId?: string }>(
      PERMISSION_RESOLVED_EVENT,
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

  if (requests.length === 0) {
    return null;
  }

  const dismissRequest = (requestId: string) =>
    setRequests((current) => current.filter((q) => q.requestId !== requestId));

  return (
    <div className={styles.stack} aria-live="polite">
      {requests.map((req, cardIndex) => (
        <ApprovalCard
          key={req.requestId}
          ref={cardIndex === 0 ? firstCardRef : null}
          req={req}
          onDecided={dismissRequest}
        />
      ))}
    </div>
  );
};

export default InlineApprovalCard;
