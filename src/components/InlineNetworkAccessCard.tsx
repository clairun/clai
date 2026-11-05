import { Fragment, useEffect, useRef, useState, type ReactNode, type Ref } from 'react';
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
import { chipText, listOf, quoted } from './requestChips';
import { DenyReasonPanel, useDenial } from './DenyReasonPanel';
import { useDecisionSender } from './useDecisionSender';
import styles from './InlineApprovalCard.module.css';
import grantStyles from './InlinePathGrantCard.module.css';

const NETWORK_ACCESS_REQUEST_EVENT = 'network-access://request';
// The request left the queue: a decision (here or in another view), a
// cancelled or ended run, or the agent asked again. Drop the card.
const NETWORK_ACCESS_RESOLVED_EVENT = 'network-access://resolved';


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
  const { send, isSubmitting, error } = useDecisionSender(
    (decision: NetworkAccessDecision) => submitNetworkAccessDecision(req.requestId, decision),
    () => onDecided?.(req.requestId),
  );
  const denial = useDenial();
  const [isCollapsed, setCollapsed] = useState(false);
  const spoken = Boolean(speaker);
  const agentName = requester?.name ?? 'this agent';

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
              {...denial.denyButton}
              className={`${styles.btn} ${styles.btnDenyOnce} ${denial.open ? styles.btnSelected : ''}`}
              disabled={isSubmitting}
              {...chipText(spoken, 'Deny', 'Deny')}
            />
          </div>
          {denial.open && (
            <DenyReasonPanel
              id={denial.panelId}
              confirmLabel="Deny access"
              submitting={isSubmitting}
              onConfirm={(reason) => send(reason ? { kind: 'deny', reason } : { kind: 'deny' })}
              onClose={denial.close}
            />
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
