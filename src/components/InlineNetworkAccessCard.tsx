import { Fragment, useEffect, useId, useRef, useState, type ReactNode, type Ref } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  listPendingNetworkAccessRequests,
  submitNetworkAccessDecision,
} from '../permissions/networkAccessClient';
import type {
  HostLogEntry,
  NetworkAccessDecision,
  NetworkAccessRequest,
  WorkspaceAgentResponse,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import { requestingAgent, type SenderAgent } from './AssistantChat/messageSender';
import RequestingAgent from './Agents/RequestingAgent';
import { chipText } from './requestChips';
import styles from './InlineApprovalCard.module.css';
import grantStyles from './InlinePathGrantCard.module.css';

const NETWORK_ACCESS_REQUEST_EVENT = 'network-access://request';
// The request left the queue: a decision (here or in another view), a
// cancelled or ended run, or the agent asked again. Drop the card.
const NETWORK_ACCESS_RESOLVED_EVENT = 'network-access://resolved';

// The backend accepts at most 500 characters.
const MAX_REASON_LENGTH = 500;
const limitReason = (value: string) => Array.from(value).slice(0, MAX_REASON_LENGTH).join('');

const quoted = (name: string) => `“${name}”`;

/** "a", "a and b", "a, b and c". */
const listOf = (items: ReactNode[]) =>
  items.map((item, i) => (
    <Fragment key={i}>
      {i === 0 ? '' : i === items.length - 1 ? ' and ' : ', '}
      {item}
    </Fragment>
  ));

const hostWithPort = (req: NetworkAccessRequest) => (req.port ? `${req.host}:${req.port}` : req.host);

/** Where “Always” saves, as for the ask-first card. */
const SaveScope = ({ req, workspaceName }: { req: NetworkAccessRequest; workspaceName?: string }) => {
  if (!req.workspaceId || !req.agentId) {
    return <>Nothing is saved: an allowed host stays allowed for the rest of this run only.</>;
  }
  const saves = req.suggestedWildcard ? (
    <>
      “Always” and “Allow {req.suggestedWildcard}” add their entry
    </>
  ) : (
    <>
      “Always” adds <code>{req.host}</code>
    </>
  );
  const here = workspaceName ? quoted(workspaceName) : null;
  if (!req.persistsToSharedAgent) {
    return (
      <>
        {saves} to this agent's allowlist in {here ?? 'this workspace'} only.
      </>
    );
  }
  const others = req.alsoAffectsWorkspaces.map(quoted);
  return (
    <>
      {saves} to this shared agent's allowlist in every workspace that has it, now and later.
      {others.length > 0 && <> Today that is {listOf([here ?? 'this one', ...others])}.</>}
    </>
  );
};

/** “This run: github.com ✓ · example.org ✕” */
const RunHosts = ({ hosts }: { hosts: readonly HostLogEntry[] }) =>
  hosts.length === 0 ? null : (
    <p className={styles.segmentHint}>
      This run:{' '}
      {hosts.map((entry, i) => (
        <Fragment key={`${entry.host}-${entry.allowed}`}>
          {i > 0 && ' · '}
          {entry.host}{' '}
          <span role="img" aria-label={entry.allowed ? 'allowed' : 'refused'}>
            {entry.allowed ? '✓' : '✕'}
          </span>
        </Fragment>
      ))}
    </p>
  );

interface NetworkAccessCardProps {
  req: NetworkAccessRequest;
  /** Draw the request as the agent saying it (the Fleet): no tag or collapse, short chips. */
  speaker?: (prompt: ReactNode) => ReactNode;
  /** The agent that asked, drawn in the header; the spoken card draws its own face. */
  requester?: SenderAgent | null;
  /** Names the request's workspace where “this workspace” would be unclear (the Fleet). */
  workspaceName?: string;
  /** The decision was accepted. */
  onDecided?: (requestId: string) => void;
  ref?: Ref<HTMLElement>;
}

/** One network access request and its decision. */
export const NetworkAccessCard = ({ req, speaker, requester, workspaceName, onDecided, ref }: NetworkAccessCardProps) => {
  const [isSubmitting, setSubmitting] = useState(false);
  // Latched once the decision is sent, so it cannot be sent twice.
  const sent = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [denying, setDenying] = useState(false);
  const [reason, setReason] = useState('');
  const [isCollapsed, setCollapsed] = useState(false);
  const reasonId = useId();
  const reasonHintId = `${reasonId}-hint`;
  const reasonCountId = `${reasonId}-count`;
  const denialId = `${reasonId}-denial`;
  const denyRef = useRef<HTMLButtonElement>(null);
  const reasonRef = useRef<HTMLTextAreaElement>(null);
  const reasonLength = Array.from(reason).length;
  const spoken = Boolean(speaker);
  const agentName = requester?.name ?? 'this agent';

  const send = async (decision: NetworkAccessDecision) => {
    if (sent.current) return;
    sent.current = true;
    setSubmitting(true);
    setError(null);
    try {
      await submitNetworkAccessDecision(req.requestId, decision);
      onDecided?.(req.requestId);
    } catch (e) {
      // A refused or failed decision leaves the request pending: answer again.
      sent.current = false;
      setError(typeof e === 'string' ? e : e instanceof Error ? e.message : 'Failed to submit decision');
    } finally {
      setSubmitting(false);
    }
  };

  useEffect(() => {
    if (denying) reasonRef.current?.focus();
  }, [denying]);

  const closeDenial = () => {
    setDenying(false);
    denyRef.current?.focus();
  };

  const confirmDenial = () => {
    const why = reason.trim();
    send(why ? { kind: 'deny', reason: why } : { kind: 'deny' });
  };

  return (
    <article
      ref={ref}
      className={spoken ? `${styles.card} ${styles.spoken}` : styles.card}
      aria-label={requester ? `Network access requested by ${requester.name}` : 'Network access requested'}
    >
      {!spoken && (
        <header className={styles.cardHeader}>
          <div className={styles.cardHeaderLeft}>
            <span className={styles.tag}>Network access requested</span>
            <span className={styles.cardSubtitle}>
              {requester ? (
                <>
                  <RequestingAgent agent={requester} /> wants network access:
                </>
              ) : (
                'An agent wants network access:'
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
        <div className={styles.collapsedSummary}>{hostWithPort(req)}</div>
      ) : (
        <div className={styles.cardBody}>
          {speaker ? (
            speaker(
              <>
                May I reach <code>{hostWithPort(req)}</code>?
                <span className={grantStyles.spokenReason}>{req.reason || '(no reason given)'}</span>
              </>,
            )
          ) : (
            <>
              <div className={styles.segmentText}>
                <code>{hostWithPort(req)}</code>
                <span className={styles.ruleChip}>not on allowlist</span>
              </div>
              <div className={grantStyles.reasonBlock}>
                <span className={grantStyles.reasonLabel}>Reason from agent:</span>
                <div className={grantStyles.reasonText}>{req.reason || '(no reason given)'}</div>
              </div>
            </>
          )}
          <RunHosts hosts={req.runHosts} />
          <p className={styles.segmentHint}>
            <SaveScope req={req} workspaceName={workspaceName} />
          </p>
          <div className={styles.segmentButtons}>
            <button
              type="button"
              className={`${styles.btn} ${styles.btnAllowOnce}`}
              onClick={() => send({ kind: 'allowRun' })}
              disabled={isSubmitting}
              {...chipText(spoken, 'Allow this run', 'This run', 'A later run asks again.')}
            />
            <button
              type="button"
              className={`${styles.btn} ${styles.btnAllow}`}
              onClick={() => send({ kind: 'allowAlways', entry: req.host })}
              disabled={isSubmitting}
              {...chipText(spoken, `Always for ${agentName}`, 'Always', `Adds ${req.host} to the allowlist.`)}
            />
            {req.suggestedWildcard && (
              <button
                type="button"
                className={`${styles.btn} ${styles.btnAllow}`}
                onClick={() => send({ kind: 'allowAlways', entry: req.suggestedWildcard! })}
                disabled={isSubmitting}
                {...chipText(
                  spoken,
                  `Allow ${req.suggestedWildcard}`,
                  req.suggestedWildcard,
                  `Adds ${req.suggestedWildcard} to the allowlist: every host under it.`,
                )}
              />
            )}
            <button
              type="button"
              ref={denyRef}
              className={`${styles.btn} ${styles.btnDenyOnce} ${denying ? styles.btnSelected : ''}`}
              onClick={() => (denying ? closeDenial() : setDenying(true))}
              disabled={isSubmitting}
              aria-expanded={denying}
              aria-controls={denying ? denialId : undefined}
              {...chipText(spoken, 'Deny', 'Deny')}
            />
          </div>
          {denying && (
            <div
              id={denialId}
              className={styles.denialArea}
              onKeyDown={(e) => {
                if (e.key === 'Escape' && !isSubmitting) {
                  e.stopPropagation();
                  closeDenial();
                }
              }}
            >
              <div className={styles.reasonField}>
                <label htmlFor={reasonId}>Reason for denying (optional)</label>
                <textarea
                  ref={reasonRef}
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
                  {isSubmitting ? 'Submitting…' : 'Deny access'}
                </button>
                <button type="button" className={styles.btn} onClick={closeDenial} disabled={isSubmitting}>
                  Cancel
                </button>
              </div>
            </div>
          )}
          {error && !isSubmitting && (
            <div className={styles.error} role="alert">
              {error}
            </div>
          )}
        </div>
      )}
    </article>
  );
};

/**
 * The workspace's pending network access requests, one card each, beside
 * the ask-first and path cards: seeded from the backend on mount, kept by
 * the request and resolved events.
 */
const InlineNetworkAccessCard = ({
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
  const [requests, setRequests] = useState<NetworkAccessRequest[]>([]);
  const firstCardRef = useRef<HTMLElement | null>(null);
  const previousCountRef = useRef(0);

  useEffect(() => {
    // The Workspace page reuses this instance across workspaces: drop the
    // previous workspace's requests before seeding and subscribing.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- subscription+seed effect: clear stale per-workspace state on workspaceId change, then seed from the pending list and subscribe; the lint rule cannot model the seed/listen interaction.
    setRequests([]);
    previousCountRef.current = 0;

    if (!workspaceId) return undefined;

    let cancelled = false;
    const add = (incoming: NetworkAccessRequest[]) =>
      setRequests((current) => {
        const known = new Set(current.map((q) => q.requestId));
        const additions = incoming.filter((q) => !known.has(q.requestId));
        return additions.length === 0 ? current : [...current, ...additions];
      });

    listPendingNetworkAccessRequests(workspaceId)
      .then((pending) => {
        if (!cancelled && Array.isArray(pending)) add(pending);
      })
      .catch(() => {
        // Non-fatal: events still bring new requests.
      });

    const unlistenPromise = listen<NetworkAccessRequest>(NETWORK_ACCESS_REQUEST_EVENT, (event) => {
      // The unlisten below is async: a stale listener may still fire after
      // a workspace switch, so the cancelled flag guards synchronously.
      if (cancelled) return;
      const req = event.payload;
      if (!req?.requestId || !req.host || req.workspaceId !== workspaceId) return;
      add([req]);
    });
    const unlistenResolvedPromise = listen<{ requestId?: string }>(NETWORK_ACCESS_RESOLVED_EVENT, (event) => {
      if (cancelled) return;
      const requestId = event.payload?.requestId;
      if (requestId) setRequests((current) => current.filter((q) => q.requestId !== requestId));
    });

    return () => {
      cancelled = true;
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
      unlistenResolvedPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [workspaceId]);

  // Scroll into view on the first arrival only.
  useEffect(() => {
    const prev = previousCountRef.current;
    const next = requests.length;
    previousCountRef.current = next;
    if (prev === 0 && next > 0 && firstCardRef.current) {
      firstCardRef.current.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }
  }, [requests.length]);

  if (requests.length === 0) return null;

  const dismissRequest = (requestId: string) =>
    setRequests((current) => current.filter((q) => q.requestId !== requestId));

  return (
    <div className={styles.stack} aria-live="polite">
      {requests.map((req, cardIndex) => (
        <NetworkAccessCard
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

export default InlineNetworkAccessCard;
