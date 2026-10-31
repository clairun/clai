import React, { useEffect, useRef, useState, type ReactNode, type Ref } from 'react';
import { listen } from '@tauri-apps/api/event';
import { listPendingPermissionRequests, submitPermissionDecision } from '../permissions/client';
import type { PermissionRequest, SegmentApproval, SegmentDecision } from '../generated/bindings';
import { chipText } from './requestChips';
import styles from './InlineApprovalCard.module.css';

const PERMISSION_REQUEST_EVENT = 'permissions://request';
// A pending request left the queue: a decision (here or in another view), a
// cancelled or ended run, or a re-asked command superseded it. Drop the
// card. Payload: { requestId }.
const PERMISSION_RESOLVED_EVENT = 'permissions://resolved';

const allowOnce = (): SegmentDecision => ({ kind: 'allowOnce' });
const denyOnce = (): SegmentDecision => ({ kind: 'denyOnce' });
const allowAlways = (prefix: string): SegmentDecision => ({ kind: 'allowAlways', scope: 'agent', prefix });
const denyAlways = (prefix: string): SegmentDecision => ({ kind: 'denyAlways', scope: 'agent', prefix });

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

interface SegmentCellState {
  prefix: string;
  decision: DecisionKind | null;
}

type CardState = Record<number, SegmentCellState>;

// How a row's chosen decision reads in the bulk note.
const DECISION_LABEL: Record<DecisionKind, string> = {
  allowOnce: 'Allow once',
  denyOnce: 'Deny once',
  allowAlways: 'Always allow',
  denyAlways: 'Always deny',
};

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
  const [error, setError] = useState<string | null>(null);
  // Collapse: squish the card to a one-line summary so a tall (or stacked)
  // request can't swallow the conversation. Default expanded.
  const [isCollapsed, setCollapsed] = useState(false);
  const spoken = Boolean(speaker);

  const sendDecisions = async (decisions: SegmentDecision[]) => {
    setSubmitting(true);
    setError(null);
    try {
      await submitPermissionDecision(req.requestId, decisions);
      onDecided?.(req.requestId);
    } catch (e) {
      setError(typeof e === 'string' ? e : e instanceof Error ? e.message : 'Failed to submit decision');
    } finally {
      setSubmitting(false);
    }
  };

  const setSegmentPrefix = (idx: number, prefix: string) =>
    setCells((current) => ({ ...current, [idx]: { decision: current[idx]?.decision ?? null, prefix } }));

  // The prefix a part is saved under: what the user typed in the field,
  // else the backend's suggestion. Empty means the part is not saved as
  // a rule at all. One place, so what the card promises and what it
  // submits cannot drift apart.
  const resolvedPrefix = (seg: SegmentApproval, idx: number): string =>
    (cells[idx]?.prefix ?? seg.suggestedPrefix ?? '').trim();

  // What a row's chosen kind means right now. An "always allow" whose
  // prefix was cleared has nothing to save, so it means an allow-once. A
  // deny rule always has a prefix: with no stable one it blocks the
  // command text itself.
  const effectiveDecision = (
    seg: SegmentApproval,
    idx: number,
    kind: DecisionKind | null
  ): DecisionKind | null => {
    if (kind === null) return null;
    if (kind === 'allowAlways' && resolvedPrefix(seg, idx).length === 0) return 'allowOnce';
    return kind;
  };

  const decisionFor = (seg: SegmentApproval, idx: number, kind: DecisionKind): SegmentDecision => {
    switch (effectiveDecision(seg, idx, kind)) {
      case 'allowOnce':
        return allowOnce();
      case 'denyOnce':
        return denyOnce();
      case 'allowAlways':
        return allowAlways(resolvedPrefix(seg, idx));
      default:
        return denyAlways(resolvedPrefix(seg, idx) || seg.text);
    }
  };

  const handleSegmentDecision = (idx: number, kind: DecisionKind) => {
    const next: CardState = { ...cells, [idx]: { prefix: cells[idx]?.prefix ?? '', decision: kind } };
    setCells(next);
    if (!req.segments.every((_, i) => next[i]?.decision)) return;
    sendDecisions(req.segments.map((seg, i) => decisionFor(seg, i, next[i]!.decision!)));
  };

  const denyAll = () => sendDecisions(req.segments.map(() => denyOnce()));

  // Batch "trust this whole command" shortcut. It decides the parts the
  // user has not decided yet: those with a prefix (either Simple or
  // Opaque whose head is recognized) get AllowAlways with the user's
  // edited prefix or the suggestion, the rest fall back to AllowOnce so
  // the run still proceeds. The parts the user already decided keep
  // their decision, so a deny is never turned into an allow rule by a
  // later click.
  const allowAllAlways = () =>
    sendDecisions(
      req.segments.map((seg, idx) => {
        const decided = cells[idx]?.decision;
        if (decided) return decisionFor(seg, idx, decided);
        const prefix = resolvedPrefix(seg, idx);
        return prefix ? allowAlways(prefix) : allowOnce();
      })
    );

  // What the bulk click will do, part by part: it decides the parts the
  // user has not decided yet and leaves the decided ones alone. A part
  // with a prefix is saved as an allow rule, a part without one is
  // allowed for this run only. The note under the button says exactly
  // that, so the click never decides more than it says.
  const bulkPlan = req.segments.map((seg, idx) => {
    const decided = cells[idx]?.decision ?? null;
    return {
      idx,
      text: seg.text,
      prefix: resolvedPrefix(seg, idx),
      decided,
      effective: effectiveDecision(seg, idx, decided),
    };
  });
  const pendingParts = bulkPlan.filter((part) => part.decided === null);
  const keptParts = bulkPlan.filter((part) => part.decided !== null);
  const savedParts = pendingParts.filter((part) => part.prefix.length > 0);
  const onceOnlyParts = pendingParts.filter((part) => part.prefix.length === 0);
  // One rule for both cards: the bulk action is offered only while it
  // can save something. When no pending part has a prefix to save, the
  // click would decide the rest for this run only, so the card offers
  // one chip per remaining part instead. The Fleet card and the
  // workspace card therefore always offer the same choices.
  const showsBulkChip = pendingParts.length > 0 && savedParts.length > 0;
  const hasBulkNote = onceOnlyParts.length > 0 || keptParts.length > 0;
  const bulkNoteId = `approval-bulk-note-${req.requestId}`;
  const bulkScope = keptParts.length > 0 ? 'the rest' : 'all';
  const bulkLead = keptParts.length > 0 ? 'Rest' : 'All';
  const bulkFullLabel =
    onceOnlyParts.length === 0
      ? `Allow ${bulkScope} (always)`
      : `Allow ${bulkScope} (always where possible)`;
  const bulkShortLabel =
    onceOnlyParts.length === 0
      ? `${bulkLead} always`
      : `${bulkLead} · ${savedParts.length} of ${pendingParts.length} saved`;
  const bulkWhy =
    onceOnlyParts.length === 0
      ? 'Saves the prefix of every part this click decides and runs the command.'
      : `Saves the prefixes of ${savedParts.length} of the ${pendingParts.length} parts this click decides and runs the command; the remaining ${onceOnlyParts.length === 1 ? 'part is' : 'parts are'} allowed once only.`;
  // Parts are named by their position in the command: two parts with the
  // same text stay apart, and a part without text still gets a name.
  const partName = (part: { text: string }) => part.text.trim() || '(empty)';
  // What the click does with a part the user already decided: it keeps the
  // decision, and an "always" decision is a rule, so this click saves it
  // too. The note has to say so, or it under-reports what is persisted.
  const keptTail = (part: (typeof bulkPlan)[number]) => {
    if (part.effective === 'allowAlways')
      return (
        <>
          saved as an allow rule for <code className={styles.bulkNotePart}>{part.prefix}</code>
        </>
      );
    if (part.effective === 'denyAlways')
      return (
        <>
          saved as a deny rule for{' '}
          <code className={styles.bulkNotePart}>{part.prefix || partName(part)}</code>
        </>
      );
    return <>for this run only</>;
  };
  const bulkNote = (
    <>
      <span>
        This click saves:{' '}
        {savedParts.map((part, i) => (
          <span key={part.idx}>
            {i > 0 ? ', ' : ''}
            {part.idx + 1}) <code className={styles.bulkNotePart}>{part.prefix}</code>
          </span>
        ))}
        .{' '}
      </span>
      {onceOnlyParts.length > 0 && (
        <span>
          Allowed once only:{' '}
          {onceOnlyParts.map((part, i) => (
            <span key={part.idx}>
              {i > 0 ? ', ' : ''}
              {part.idx + 1}) <code className={styles.bulkNotePart}>{partName(part)}</code>
            </span>
          ))}
          .{' '}
        </span>
      )}
      {keptParts.length > 0 && (
        <span>
          Keeps your decision on{' '}
          {keptParts.map((part, i) => (
            <span key={part.idx}>
              {i > 0 ? ', ' : ''}
              {part.idx + 1}) <code className={styles.bulkNotePart}>{partName(part)}</code>{' '}
              ({DECISION_LABEL[part.effective!]}), {keptTail(part)}
            </span>
          ))}
          .{' '}
        </span>
      )}
    </>
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
      : {
          title: `${savedOn}. ${
            verb === 'allow'
              ? `Saves the prefix \`${prefix}\`.`
              : `Blocks commands starting with \`${prefix}\`.`
          }`,
          children: `Always ${verb} (${reach})`,
        };

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
                  ? 'Choose how to handle this command:'
                  : `${req.segments.length} parts need a decision (submits as soon as every part is decided):`}
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
              // The prefix this part would be saved under. Without one
              // the part cannot be saved as an allow rule, so the card
              // offers no "always allow" and says what allowing means.
              const savePrefix = resolvedPrefix(seg, idx);
              const canSaveRule = savePrefix.length > 0;
              // A deny rule needs something to name: the prefix, or the
              // command text when there is no prefix. With neither, the
              // rule would be saved under an empty prefix, which the
              // backend skips, so the card offers no "always deny" either.
              const denyRuleName = savePrefix || seg.text;
              const canSaveDenyRule = denyRuleName.trim().length > 0;
              // The decision this row will submit: an "always allow" with
              // an emptied prefix field saves nothing, so it is an
              // allow-once.
              const rowDecision = effectiveDecision(seg, idx, cell.decision);
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
                      className={`${styles.btn} ${styles.btnAllowOnce} ${rowDecision === 'allowOnce' ? styles.btnSelected : ''}`}
                      onClick={() => handleSegmentDecision(idx, 'allowOnce')}
                      disabled={isSubmitting}
                      {...chipText(spoken, 'Allow once', 'Once')}
                    />
                    {canSaveRule && (
                      <button
                        type="button"
                        className={`${styles.btn} ${styles.btnAllow} ${rowDecision === 'allowAlways' ? styles.btnSelected : ''}`}
                        onClick={() => handleSegmentDecision(idx, 'allowAlways')}
                        disabled={isSubmitting}
                        {...alwaysChip('allow', savePrefix)}
                      />
                    )}
                    {(canPersist || !spoken) && canSaveDenyRule && (
                      <button
                        type="button"
                        className={`${styles.btn} ${styles.btnDeny} ${rowDecision === 'denyAlways' ? styles.btnSelected : ''}`}
                        onClick={() => handleSegmentDecision(idx, 'denyAlways')}
                        disabled={isSubmitting}
                        {...alwaysChip('deny', denyRuleName)}
                      />
                    )}
                    <button
                      type="button"
                      className={`${styles.btn} ${styles.btnDenyOnce} ${rowDecision === 'denyOnce' ? styles.btnSelected : ''}`}
                      onClick={() => handleSegmentDecision(idx, 'denyOnce')}
                      disabled={isSubmitting}
                      {...chipText(spoken, 'Deny once', 'Deny')}
                    />
                  </div>
                  {!canSaveRule && (
                    <p className={styles.segmentHint}>
                      {spoken
                        ? 'No prefix to save for this part, so allowing it applies to this run only.'
                        : canSaveDenyRule
                          ? 'No prefix to save for this part, so allowing it applies to this run only. Always deny still blocks commands starting with this text.'
                          : 'No prefix to save for this part, so allowing it applies to this run only. There is no command text to name either, so no rule can be saved for this part.'}
                    </p>
                  )}
                </div>
              );
            })}
          </section>
          {error && !isSubmitting && <div className={styles.error}>{error}</div>}
          {req.segments.length > 1 && (
            <footer className={styles.cardFooter}>
              {showsBulkChip && hasBulkNote && (
                <p className={styles.bulkNote} id={bulkNoteId}>
                  {bulkNote}
                </p>
              )}
              {showsBulkChip && (
                <button
                  type="button"
                  className={`${styles.btn} ${styles.btnAllow}`}
                  onClick={allowAllAlways}
                  disabled={isSubmitting}
                  aria-describedby={hasBulkNote ? bulkNoteId : undefined}
                  {...chipText(spoken, bulkFullLabel, bulkShortLabel, bulkWhy)}
                />
              )}
              <button
                type="button"
                className={`${styles.btn} ${styles.btnDenyAll}`}
                onClick={denyAll}
                disabled={isSubmitting}
                {...chipText(spoken, 'Deny entire command', 'Deny all', 'Deny every part of this pipeline in one click')}
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
