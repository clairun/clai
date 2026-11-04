import { Fragment, useEffect, useId, useRef, useState, type ReactNode, type Ref } from 'react';
import { listen } from '@tauri-apps/api/event';
import { listPendingPermissionRequests, submitPermissionDecision } from '../permissions/client';
import type {
  AskFirstDecision,
  PermissionRequest,
  WorkspaceAgentResponse,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import { requestingAgent, type SenderAgent } from './AssistantChat/messageSender';
import RequestingAgent from './Agents/RequestingAgent';
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

/** "a", "a and b", "a, b and c". */
const listOf = (items: ReactNode[]) =>
  items.map((item, i) => (
    <Fragment key={i}>
      {i === 0 ? '' : i === items.length - 1 ? ' and ' : ', '}
      {item}
    </Fragment>
  ));

const quoted = (name: string) => `“${name}”`;

/**
 * Where "Always allow" saves: a shared teammate's definition, in force in
 * every workspace it works in, or this workspace's Main. With no workspace or
 * agent id the backend saves nothing and stops asking for the rest of the run.
 */
type AlwaysScope = 'run' | 'workspace' | 'everywhere';

const alwaysScope = (req: PermissionRequest): AlwaysScope =>
  !req.workspaceId || !req.agentId ? 'run' : req.persistsToSharedAgent ? 'everywhere' : 'workspace';

const AlwaysAllowScope = ({
  req,
  rules,
  workspaceName,
}: {
  req: PermissionRequest;
  rules: string[];
  workspaceName?: string;
}) => {
  const named = listOf(rules.map((rule) => <code key={rule}>{rule}</code>));
  const here = workspaceName ? quoted(workspaceName) : null;
  switch (alwaysScope(req)) {
    case 'run':
      return <>Always allow stops asking about {named} for the rest of this run only. Nothing is saved.</>;
    case 'workspace':
      return (
        <>
          Always allow removes {named} from this agent's ask-first list in {here ?? 'this workspace'} only.
        </>
      );
    case 'everywhere': {
      const others = req.alsoAffectsWorkspaces.map(quoted);
      return (
        <>
          Always allow removes {named} from this shared agent's ask-first list in every workspace that has it, now
          and later.
          {others.length > 0 && <> Today that is {listOf([here ?? 'this one', ...others])}.</>}
        </>
      );
    }
  }
};

interface ApprovalCardProps {
  req: PermissionRequest;
  /**
   * Draw the command as the agent saying it (the Fleet's asking face and
   * bubble): no tag or collapse, and short decision chips (full labels in
   * the tooltips) below.
   */
  speaker?: (prompt: ReactNode) => ReactNode;
  /** The agent that asked, drawn in the header; the spoken card draws its own face. */
  requester?: SenderAgent | null;
  /** Names the request's workspace where "this workspace" would be unclear (the Fleet). */
  workspaceName?: string;
  /** The decision was accepted. */
  onDecided?: (requestId: string) => void;
  ref?: Ref<HTMLElement>;
}

/** One ask-first request and its decision. */
export const ApprovalCard = ({ req, speaker, requester, workspaceName, onDecided, ref }: ApprovalCardProps) => {
  const [isSubmitting, setSubmitting] = useState(false);
  // Latched once the decision is sent, so it cannot be sent twice.
  const sent = useRef(false);
  const [error, setError] = useState<string | null>(null);
  // Deny opens the reason field; the denial is sent from its confirm.
  const [denying, setDenying] = useState(false);
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

  const rules = [...new Set(req.rows.map((row) => row.rule))];
  const scope = alwaysScope(req);
  const reach = scope === 'run' ? 'this run' : scope === 'everywhere' ? 'every workspace' : (workspaceName ?? 'this workspace');

  const send = async (decision: AskFirstDecision) => {
    // One decision per request: a click that lands after the send must not
    // send the decision again.
    if (sent.current) return;
    sent.current = true;
    setSubmitting(true);
    setError(null);
    try {
      await submitPermissionDecision(req.requestId, decision);
      onDecided?.(req.requestId);
    } catch (e) {
      // A decision that failed can be sent again.
      sent.current = false;
      setError(typeof e === 'string' ? e : e instanceof Error ? e.message : 'Failed to submit decision');
    } finally {
      setSubmitting(false);
    }
  };

  const confirmDenial = () => {
    const why = reason.trim();
    send(why ? { kind: 'deny', reason: why } : { kind: 'deny' });
  };

  return (
    <article
      ref={ref}
      className={spoken ? `${styles.card} ${styles.spoken}` : styles.card}
      aria-label={requester ? `Permission requested by ${requester.name}` : 'Permission requested'}
    >
      {!spoken && (
        <header className={styles.cardHeader}>
          <div className={styles.cardHeaderLeft}>
            <span className={styles.tag}>Permission requested</span>
            <span className={styles.cardSubtitle}>
              {requester ? (
                <>
                  <RequestingAgent agent={requester} /> wants to run:
                </>
              ) : (
                'An agent wants to run:'
              )}
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
            <p className={styles.segmentsLabel}>
              {req.rows.length === 1 ? 'It is on the ask-first list:' : 'These parts are on the ask-first list:'}
            </p>
            <ul className={styles.matchList} aria-label="Ask-first matches">
              {req.rows.map((row, idx) => (
                <li key={idx} className={styles.segmentRow}>
                  <div className={styles.segmentText}>
                    <code title={spoken ? row.text : undefined}>{row.text}</code>
                    <span className={styles.ruleChip} title={`Matches the ask-first rule \`${row.rule}\``}>
                      {row.rule}
                    </span>
                  </div>
                </li>
              ))}
            </ul>
            <p className={styles.segmentHint}>
              <AlwaysAllowScope req={req} rules={rules} workspaceName={workspaceName} />
            </p>
            <div className={styles.segmentButtons}>
              <button
                type="button"
                className={`${styles.btn} ${styles.btnAllowOnce}`}
                onClick={() => send({ kind: 'allowOnce' })}
                disabled={isSubmitting}
                {...chipText(spoken, 'Allow once', 'Once', 'Runs this command only; the next one asks again.')}
              />
              <button
                type="button"
                className={`${styles.btn} ${styles.btnAllow}`}
                onClick={() => send({ kind: 'alwaysAllow' })}
                disabled={isSubmitting}
                {...chipText(spoken, `Always allow (${reach})`, 'Always')}
              />
              <button
                type="button"
                className={`${styles.btn} ${styles.btnDenyOnce} ${denying ? styles.btnSelected : ''}`}
                onClick={() => setDenying(true)}
                disabled={isSubmitting}
                aria-expanded={denying}
                {...chipText(spoken, 'Deny', 'Deny')}
              />
            </div>
          </section>
          {denying && (
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
                  disabled={isSubmitting}
                >
                  {isSubmitting ? 'Submitting…' : 'Deny command'}
                </button>
              </div>
            </div>
          )}
          {error && !isSubmitting && <div className={styles.error}>{error}</div>}
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
const InlineApprovalCard = ({
  workspaceId,
  roster = [],
  loadedTasks,
}: {
  workspaceId: string | null;
  /** The workspace's agents, to draw who asked each request. */
  roster?: readonly WorkspaceAgentResponse[];
  /** The workspace's tasks, to find an agent removed from the roster mid-task. */
  loadedTasks?: () => readonly WorkspaceTaskResponse[];
}) => {
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
      if (!req || !req.requestId || !Array.isArray(req.rows)) return;
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
          requester={requestingAgent(req.agentId, roster, loadedTasks?.() ?? [])}
          onDecided={dismissRequest}
        />
      ))}
    </div>
  );
};

export default InlineApprovalCard;
