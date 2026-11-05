import { Fragment, useState, type ReactNode, type Ref } from 'react';
import {
  listPendingNetworkAccessRequests,
  submitNetworkAccessDecision,
} from '../permissions/networkAccessClient';
import { hostWithPort, unicodeHost } from '../permissions/hostNames';
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
import { ResolvedRequestNotice } from './ResolvedRequestNotice';
import { PendingRequestStack, type PendingRequestSource } from './PendingRequestStack';
import styles from './InlineApprovalCard.module.css';
import grantStyles from './InlinePathGrantCard.module.css';

const NETWORK_ACCESS_REQUEST_EVENT = 'network-access://request';
const NETWORK_ACCESS_RESOLVED_EVENT = 'network-access://resolved';


// The run log holds up to 200 hosts; the card names the first few.
const RUN_HOSTS_SHOWN = 8;

/** “Always” saves on the agent; the backend refuses it with no agent to save on. */
const canSave = (req: NetworkAccessRequest) => Boolean(req.workspaceId && req.agentId);

/** Where “Always” saves, as for the ask-first card. */
const SaveScope = ({ req, workspaceName }: { req: NetworkAccessRequest; workspaceName?: string }) => {
  if (!canSave(req)) {
    return <>There is no agent to save an entry on, so the host can be allowed for this run only.</>;
  }
  const saves = req.suggestedWildcard ? (
    <>
      “Always” and “Allow {unicodeHost(req.suggestedWildcard)}” add their entry
    </>
  ) : (
    <>
      “Always” adds <code>{unicodeHost(req.host)}</code>
    </>
  );
  const here = workspaceName ? quoted(workspaceName) : null;
  if (!req.persistsToSharedAgent) {
    return (
      <>
        {saves} to this agent's allowed hosts in {here ?? 'this workspace'} only.
      </>
    );
  }
  const others = req.alsoAffectsWorkspaces.map(quoted);
  return (
    <>
      {saves} to this shared agent's allowed hosts in every workspace that has it, now and later.
      {others.length > 0 && <> Today that is {listOf([here ?? 'this one', ...others])}.</>}
    </>
  );
};

/** “This run: github.com ✓ · example.org ✕ and 3 more” */
const RunHosts = ({ hosts }: { hosts: readonly HostLogEntry[] }) =>
  hosts.length === 0 ? null : (
    <p className={styles.segmentHint}>
      This run:{' '}
      {hosts.slice(0, RUN_HOSTS_SHOWN).map((entry, i) => (
        <Fragment key={`${entry.host}-${entry.allowed}`}>
          {i > 0 && ' · '}
          {unicodeHost(entry.host)}{' '}
          <span role="img" aria-label={entry.allowed ? 'allowed' : 'refused'}>
            {entry.allowed ? '✓' : '✕'}
          </span>
        </Fragment>
      ))}
      {hosts.length > RUN_HOSTS_SHOWN && ` and ${hosts.length - RUN_HOSTS_SHOWN} more`}
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
  const { send, isSubmitting, error, resolved } = useDecisionSender(
    (decision: NetworkAccessDecision) => submitNetworkAccessDecision(req.requestId, decision),
    () => onDecided?.(req.requestId),
  );
  const denial = useDenial();
  const [isCollapsed, setCollapsed] = useState(false);
  const spoken = Boolean(speaker);
  const agentName = requester?.name ?? 'this agent';
  // Names read in Unicode; the entries sent stay in the ASCII form the
  // backend compares, and the full card shows that form too, so a look-alike
  // name is not mistaken for the one it imitates.
  const host = unicodeHost(req.host);
  const asciiHost = host === req.host ? null : req.host;
  const shown = hostWithPort(host, req.port);
  const wildcard = req.suggestedWildcard ? unicodeHost(req.suggestedWildcard) : null;

  return (
    <article
      ref={ref}
      tabIndex={-1}
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
        <div className={styles.collapsedSummary}>{shown}</div>
      ) : (
        <div className={styles.cardBody}>
          {speaker ? (
            speaker(
              <>
                May I reach <code title={asciiHost ?? undefined}>{shown}</code>?
                <span className={grantStyles.spokenReason}>{req.reason || '(no reason given)'}</span>
              </>,
            )
          ) : (
            <>
              <div className={styles.segmentText}>
                <code>{shown}</code>
                {asciiHost && <span className={styles.mutedHint}>{asciiHost}</span>}
                <span className={styles.ruleChip}>not in allowed hosts</span>
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
          {resolved ? (
            <ResolvedRequestNotice onDismiss={onDecided && (() => onDecided(req.requestId))} />
          ) : (
            <div className={styles.segmentButtons}>
              <button
                type="button"
                className={`${styles.btn} ${styles.btnAllowOnce}`}
                onClick={() => send({ kind: 'allowRun' })}
                disabled={isSubmitting}
                {...chipText(spoken, 'Allow this run', 'This run', 'A later run asks again.')}
              />
              {canSave(req) && (
                <button
                  type="button"
                  className={`${styles.btn} ${styles.btnAllow}`}
                  onClick={() => send({ kind: 'allowAlways', entry: req.host })}
                  disabled={isSubmitting}
                  {...chipText(spoken, `Always for ${agentName}`, 'Always', `Adds ${host} to this agent's allowed hosts.`)}
                />
              )}
              {canSave(req) && req.suggestedWildcard && wildcard && (
                <button
                  type="button"
                  className={`${styles.btn} ${styles.btnAllow}`}
                  onClick={() => send({ kind: 'allowAlways', entry: req.suggestedWildcard! })}
                  disabled={isSubmitting}
                  {...chipText(
                    spoken,
                    `Allow ${wildcard}`,
                    wildcard,
                    `Adds ${wildcard} to this agent's allowed hosts: every host under it.`,
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
          )}
          {denial.open && !resolved && (
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

const NETWORK_ACCESS_REQUESTS: PendingRequestSource<NetworkAccessRequest> = {
  list: listPendingNetworkAccessRequests,
  requestEvent: NETWORK_ACCESS_REQUEST_EVENT,
  resolvedEvent: NETWORK_ACCESS_RESOLVED_EVENT,
  isRequest: (req) => Boolean(req.host),
};

/** The workspace's pending network access requests, beside the ask-first and path cards. */
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
  const requester = (req: { agentId: string | null }) => requestingAgent(req.agentId, roster, loadedTasks?.() ?? []);
  const requesterName = (req: { agentId: string | null }) => requester(req)?.name ?? 'An agent';
  return (
    <PendingRequestStack
      workspaceId={workspaceId}
      source={NETWORK_ACCESS_REQUESTS}
      className={styles.stack}
      announce={(req) => `${requesterName(req)} wants network access to ${hostWithPort(unicodeHost(req.host), req.port)}.`}
      renderCard={(req, card) => <NetworkAccessCard {...card} req={req} requester={requester(req)} />}
    />
  );
};

export default InlineNetworkAccessCard;
