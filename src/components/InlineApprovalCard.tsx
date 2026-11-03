import React, { useEffect, useId, useRef, useState, type ReactNode, type Ref } from 'react';
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

/**
 * Parts the backend can decide the same way twice — several commands that
 * share one suggested prefix — are answered once, so the user never has to
 * allow the same prefix twice in one request.
 */
interface SegmentGroup {
  /** The member whose cell carries the row's prefix and decision. */
  head: number;
  /** Every member's index in the request: a decision on the row lands on all
   * of them, so the submitted array stays one decision per segment. */
  indices: number[];
  /** The member texts, duplicates collapsed with a count: what the row shows. */
  members: Array<{ text: string; count: number }>;
  /** The prefix every member shares; empty when they matched on exact text. */
  suggested: string;
  /** One opaque member makes the whole row something to read carefully. */
  isOpaque: boolean;
}

// A segment with a prefix of its own groups by that prefix. The rest can only
// ever be saved by their exact text, so they group with identical text only.
const groupSegments = (segments: SegmentApproval[]): SegmentGroup[] => {
  const groups: SegmentGroup[] = [];
  const byKey = new Map<string, SegmentGroup>();
  segments.forEach((seg, idx) => {
    const suggested = (seg.suggestedPrefix ?? '').trim();
    const key = suggested ? `prefix:${suggested}` : `text:${seg.text}`;
    const group = byKey.get(key);
    if (!group) {
      const fresh: SegmentGroup = {
        head: idx,
        indices: [idx],
        members: [{ text: seg.text, count: 1 }],
        suggested,
        isOpaque: seg.kind === 'opaque',
      };
      byKey.set(key, fresh);
      groups.push(fresh);
      return;
    }
    group.indices.push(idx);
    const seen = group.members.find((member) => member.text === seg.text);
    if (seen) seen.count += 1;
    else group.members.push({ text: seg.text, count: 1 });
    group.isOpaque = group.isOpaque || seg.kind === 'opaque';
  });
  return groups;
};

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
  // One row per distinct decision: the parts that share a prefix are
  // answered together.
  const groups = groupSegments(req.segments);

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

  // A row's prefix is the prefix of every part it stands for.
  const setGroupPrefix = (group: SegmentGroup, prefix: string) =>
    setCells((current) => ({
      ...current,
      ...Object.fromEntries(
        group.indices.map((idx) => [idx, { decision: current[idx]?.decision ?? null, prefix }])
      ),
    }));

  const resolvedPrefix = (selected: CardState, seg: SegmentApproval, idx: number): string =>
    (selected[idx]?.prefix ?? seg.suggestedPrefix ?? '').trim();

  const effectiveDecision = (
    seg: SegmentApproval,
    idx: number,
    kind: DecisionKind | null
  ): DecisionKind | null => {
    if (kind === null) return null;
    if (kind === 'allowAlways' && resolvedPrefix(cells, seg, idx).length === 0) return 'allowOnce';
    return kind;
  };

  const decisionFor = (selected: CardState, seg: SegmentApproval, idx: number, why?: string): SegmentDecision => {
    const kind = selected[idx]!.decision!;
    const prefix = resolvedPrefix(selected, seg, idx);
    switch (kind) {
      case 'allowOnce':
        return allowOnce();
      case 'denyOnce':
        return denyOnce(why);
      case 'allowAlways':
        return prefix ? allowAlways(prefix) : allowOnce();
      case 'denyAlways':
        return denyAlways(prefix || seg.text, why);
    }
  };

  const decisionsFrom = (selected: CardState, why: string): SegmentDecision[] => {
    const trimmedReason = why.trim() || undefined;
    return req.segments.map((seg, i) => decisionFor(selected, seg, i, trimmedReason));
  };

  // One click decides every part of the row: the submitted array still carries
  // a decision for each of them.
  const handleGroupDecision = (group: SegmentGroup, kind: DecisionKind) => {
    const next: CardState = { ...cells };
    group.indices.forEach((idx) => {
      next[idx] = { prefix: cells[idx]?.prefix ?? '', decision: kind };
    });
    setCells(next);
    if (!groups.every((g) => next[g.head]?.decision)) return;
    // A denial waits for confirmation so the user can add a reason.
    if (groups.some((g) => isDenial(next[g.head]?.decision ?? null))) return;
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

  const hasDenial = groups.some((g) => isDenial(cells[g.head]?.decision ?? null));
  const allDecided = groups.every((g) => cells[g.head]?.decision);
  // "Deny always" writes a block rule that lasts, so the confirm says so.
  const blocksAlways = groups.some((g) => cells[g.head]?.decision === 'denyAlways');
  const confirmLabel = (): string => {
    if (isSubmitting) return 'Submitting…';
    if (groups.length > 1) return 'Submit decisions';
    return blocksAlways ? 'Deny and block always' : 'Deny command';
  };

  // Batch "trust this whole command" shortcut. It decides the rows the
  // user has not decided yet: those with a prefix (either Simple or
  // Opaque whose head is recognized) get AllowAlways with the user's
  // edited prefix or the suggestion, the rest fall back to AllowOnce so
  // the run still proceeds. The rows the user already decided keep
  // their decision, so a deny is never turned into an allow rule by a
  // later click.
  const allowAllAlways = () => {
    const next: CardState = { ...cells };
    groups.forEach((group) => {
      if (next[group.head]?.decision) return;
      const prefix = resolvedPrefix(next, req.segments[group.head]!, group.head);
      const kind: DecisionKind = prefix ? 'allowAlways' : 'allowOnce';
      group.indices.forEach((idx) => {
        next[idx] = { prefix: next[idx]?.prefix ?? '', decision: kind };
      });
    });
    if (groups.some((g) => isDenial(next[g.head]?.decision ?? null))) {
      setCells(next);
      return;
    }
    sendDecisions(decisionsFrom(next, reason));
  };

  // What the bulk click will do, row by row: it decides the rows the
  // user has not decided yet and leaves the decided ones alone. A row
  // with a prefix is saved as an allow rule, a row without one is
  // allowed for this run only. The note under the button says exactly
  // that, so the click never decides more than it says.
  const bulkPlan = groups.map((group, gidx) => {
    const first = req.segments[group.head]!;
    const decided = cells[group.head]?.decision ?? null;
    return {
      gidx,
      group,
      prefix: resolvedPrefix(cells, first, group.head),
      decided,
      effective: effectiveDecision(first, group.head, decided),
    };
  });
  type BulkPart = (typeof bulkPlan)[number];
  const pendingParts = bulkPlan.filter((part) => part.decided === null);
  const keptParts = bulkPlan.filter((part) => part.decided !== null);
  const savedParts = pendingParts.filter((part) => part.prefix.length > 0);
  const onceOnlyParts = pendingParts.filter((part) => part.prefix.length === 0);
  // One rule for both cards: the bulk action is offered only while it
  // can save something. When no pending row has a prefix to save, the
  // click would decide the rest for this run only, so the card offers
  // one chip per remaining row instead. The Fleet card and the
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
  // The click decides whole rows, so the claim it makes counts the parts
  // behind the rows, not the rows: a row that stands for two commands
  // decides two.
  const partCount = (rows: BulkPart[]) =>
    rows.reduce((total, row) => total + row.group.indices.length, 0);
  const onceOnlyCount = partCount(onceOnlyParts);
  // A row answers every command that shares its prefix, so the two
  // counts part company as soon as commands merge: five commands can need
  // three decisions. A label calls them parts only while the counts agree,
  // and names both when they do not.
  const commandCount = partCount(bulkPlan);
  const rowsMerged = groups.length < commandCount;
  const needDecisionLabel = rowsMerged
    ? `${commandCount} commands in ${groups.length} rows need a decision`
    : `${groups.length} parts need a decision`;
  const bulkWhy =
    onceOnlyParts.length === 0
      ? 'Saves the prefix of every part this click decides and runs the command.'
      : `Saves the prefixes of ${partCount(savedParts)} of the ${partCount(pendingParts)} parts this click decides and runs the command; the remaining ${onceOnlyCount === 1 ? 'part is' : 'parts are'} allowed once only.`;
  // Rows are named by their position in the card, and a row is named by
  // every command it stands for, so the note can't hide a part behind
  // its twin. A row without text still gets a name.
  const rowName = (group: SegmentGroup) => (
    <>
      {group.members.map((member, i) => (
        <React.Fragment key={member.text}>
          {i > 0 ? ', ' : ''}
          <code className={styles.bulkNotePart}>{member.text.trim() || '(empty)'}</code>
        </React.Fragment>
      ))}
    </>
  );
  // What the click does with a row the user already decided: it keeps the
  // decision, and an "always" decision is a rule, so this click saves it
  // too. The note has to say so, or it under-reports what is persisted.
  const keptTail = (part: BulkPart) => {
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
          {part.prefix ? <code className={styles.bulkNotePart}>{part.prefix}</code> : rowName(part.group)}
        </>
      );
    return <>for this run only</>;
  };
  const bulkNote = (
    <>
      <span>
        This click saves:{' '}
        {savedParts.map((part, i) => (
          <span key={part.gidx}>
            {i > 0 ? ', ' : ''}
            {part.gidx + 1}) <code className={styles.bulkNotePart}>{part.prefix}</code>
          </span>
        ))}
        .{' '}
      </span>
      {onceOnlyParts.length > 0 && (
        <span>
          Allowed once only:{' '}
          {onceOnlyParts.map((part, i) => (
            <span key={part.gidx}>
              {i > 0 ? ', ' : ''}
              {part.gidx + 1}) {rowName(part.group)}
            </span>
          ))}
          .{' '}
        </span>
      )}
      {keptParts.length > 0 && (
        <span>
          Keeps your decision on{' '}
          {keptParts.map((part, i) => (
            <span key={part.gidx}>
              {i > 0 ? ', ' : ''}
              {part.gidx + 1}) {rowName(part.group)}{' '}
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

  // Several commands left with no prefix to share are blocked each by its own
  // text, which the row above the chips lists.
  const manyDenyChip = (count: number) =>
    spoken
      ? {
          title: `Always deny the ${count} commands of this row, each by its own text (${reach})`,
          children: (
            <>
              Never · <code>{count} commands</code>
            </>
          ),
        }
      : {
          title: `${savedOn}. Blocks the ${count} commands of this row, each by its own text.`,
          children: `Always deny (${reach})`,
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
                {groups.length === 1
                  ? 'Choose how to handle this command (a denial needs a confirm):'
                  : `${needDecisionLabel} (confirm after choosing any denial):`}
              </p>
            )}
            {groups.map((group) => {
              const cell: SegmentCellState = cells[group.head] || { prefix: '', decision: null };
              const first = req.segments[group.head]!;
              // The backend now fills `suggestedPrefix` for Opaque
              // segments too (the binary head IS a stable identifier
              // even when the surrounding syntax is opaque). The
              // persistence affordances key off the prefix being
              // non-empty rather than the kind tag, so any Opaque
              // segment whose head was recognized gets the same
              // "Always allow" workflow as Simple ones.
              const canPersist = group.suggested.length > 0;
              // The prefix this row would be saved under. Without one
              // the row cannot be saved as an allow rule, so the card
              // offers no "always allow" and says what allowing means.
              const savePrefix = resolvedPrefix(cells, first, group.head);
              const canSaveRule = savePrefix.length > 0;
              // A deny rule needs something to name: the shared prefix, or
              // with none the command text of each member. With neither the
              // backend would skip an empty rule, so no "always deny" is
              // offered either.
              const denyNames = canSaveRule
                ? [savePrefix]
                : group.members
                    .map((member) => member.text.trim())
                    .filter((name) => name.length > 0);
              const canSaveDenyRule = denyNames.length > 0;
              // The decision this row will submit: an "always allow" with
              // an emptied prefix field saves nothing, so it is an
              // allow-once.
              const rowDecision = effectiveDecision(first, group.head, cell.decision);
              // The whole command is already above the rows, so one row that
              // stands for one part shows no text; a row that stands for
              // several parts shows them all, and an opaque row is tagged.
              const showsRow = groups.length > 1 || group.members.length > 1 || group.isOpaque;
              const showsTexts = groups.length > 1 || group.members.length > 1;
              return (
                <div key={group.head} className={styles.segmentRow}>
                  {showsRow && (
                    <div className={styles.segmentText}>
                      {showsTexts && (
                        <div className={styles.segmentTexts}>
                          {group.members.map((member) => (
                            <span key={member.text} className={styles.segmentMember}>
                              <code title={spoken ? member.text : undefined}>{member.text}</code>
                              {member.count > 1 && (
                                <span className={styles.memberCount} title={`${member.count} parts of this command`}>
                                  ×{member.count}
                                </span>
                              )}
                            </span>
                          ))}
                        </div>
                      )}
                      {group.isOpaque && (
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
                        onChange={(e) => setGroupPrefix(group, e.target.value)}
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
                      onClick={() => handleGroupDecision(group, 'allowOnce')}
                      disabled={isSubmitting}
                      {...chipText(spoken, 'Allow once', 'Once')}
                    />
                    {canSaveRule && (
                      <button
                        type="button"
                        className={`${styles.btn} ${styles.btnAllow} ${rowDecision === 'allowAlways' ? styles.btnSelected : ''}`}
                        onClick={() => handleGroupDecision(group, 'allowAlways')}
                        disabled={isSubmitting}
                        {...alwaysChip('allow', savePrefix)}
                      />
                    )}
                    {(canPersist || !spoken) && canSaveDenyRule && (
                      <button
                        type="button"
                        className={`${styles.btn} ${styles.btnDeny} ${rowDecision === 'denyAlways' ? styles.btnSelected : ''}`}
                        onClick={() => handleGroupDecision(group, 'denyAlways')}
                        disabled={isSubmitting}
                        {...(denyNames.length === 1
                          ? alwaysChip('deny', denyNames[0]!)
                          : manyDenyChip(denyNames.length))}
                      />
                    )}
                    <button
                      type="button"
                      className={`${styles.btn} ${styles.btnDenyOnce} ${rowDecision === 'denyOnce' ? styles.btnSelected : ''}`}
                      onClick={() => handleGroupDecision(group, 'denyOnce')}
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
                {!allDecided && (
                  <span className={styles.mutedHint}>
                    Choose for every {rowsMerged ? 'row' : 'part'} to continue
                  </span>
                )}
              </div>
            </div>
          )}
          {error && !isSubmitting && <div className={styles.error}>{error}</div>}
          {groups.length > 1 && (
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
