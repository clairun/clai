import { Fragment, useState, type ReactNode, type Ref } from 'react';
import {
  listPendingNetworkAccessRequests,
  submitNetworkAccessDecision,
} from '../permissions/networkAccessClient';
import { hostWithPort, isLoopbackHost, requestTarget, unicodeHost } from '../permissions/hostNames';
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
import { DenyReasonField, useDenyReason } from './DenyReasonField';
import { useDecisionSender } from './useDecisionSender';
import { ResolvedRequestNotice } from './ResolvedRequestNotice';
import { PendingRequestStack, type PendingRequestSource } from './PendingRequestStack';
import {
  LocalListenerDetails,
  ServedBy,
  SpokenListener,
  allowRunTitle,
  alwaysTitle,
  isSavable,
} from './LocalListenerDetails';
import styles from './InlineApprovalCard.module.css';
import grantStyles from './InlinePathGrantCard.module.css';

const NETWORK_ACCESS_REQUEST_EVENT = 'network-access://request';
const NETWORK_ACCESS_RESOLVED_EVENT = 'network-access://resolved';


// The run log holds up to 200 hosts; the card names the first few.
const RUN_HOSTS_SHOWN = 8;

/** “Always” saves on the agent; the backend refuses it with no agent to save on. */
const canSave = (req: NetworkAccessRequest) => Boolean(req.workspaceId && req.agentId);

/** The listener and port of a request for a port on this computer. */
const localOf = (req: NetworkAccessRequest) =>
  req.localListener && req.port ? { listener: req.localListener, port: req.port } : null;

/** Where “Always” saves, as for the ask-first card. */
const SaveScope = ({ req, workspaceName }: { req: NetworkAccessRequest; workspaceName?: string }) => {
  const local = localOf(req);
  const listener = local?.listener;
  if (!canSave(req)) {
    return <>There is no agent to save an entry on, so the {local ? 'port' : 'host'} can be allowed for this run only.</>;
  }
  if (listener && !isSavable(listener)) {
    return <>CLAI cannot save a rule for this listener, so the port can be allowed for this run only, until what listens there changes.</>;
  }
  const list = local ? 'allowed local ports' : 'allowed hosts';
  const saves = local && listener ? (
    <>
      “Always” adds port {local.port} for <ServedBy listener={listener} />
    </>
  ) : req.suggestedWildcard ? (
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
        {saves} to this agent's {list} in {here ?? 'this workspace'} only.
      </>
    );
  }
  const others = req.alsoAffectsWorkspaces.map(quoted);
  return (
    <>
      {saves} to this shared agent's {list} in every workspace that has it, now and later.
      {others.length > 0 && <> Today that is {listOf([here ?? 'this one', ...others])}.</>}
    </>
  );
};

/** “This run: github.com ✓ · port 5432 on this computer ✕ and 3 more” */
const RunHosts = ({ hosts }: { hosts: readonly HostLogEntry[] }) =>
  hosts.length === 0 ? null : (
    <p className={styles.segmentHint}>
      This run:{' '}
      {hosts.slice(0, RUN_HOSTS_SHOWN).map((entry, i) => (
        <Fragment key={`${entry.host}-${entry.port}-${entry.allowed}`}>
          {i > 0 && ' · '}
          {requestTarget(entry.host, isLoopbackHost(entry.host) ? entry.port : null)}{' '}
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
  // As on the ask-first card: one-click Deny, with the reason field on the full card only.
  const denyReason = useDenyReason();
  const [isCollapsed, setCollapsed] = useState(false);
  const spoken = Boolean(speaker);
  const reasonShown = !spoken && !resolved;
  const agentName = requester?.name ?? 'this agent';
  // Names read in Unicode; the entries sent stay in the ASCII form the
  // backend compares, and the full card shows that form too, so a look-alike
  // name is not mistaken for the one it imitates.
  const host = unicodeHost(req.host);
  const asciiHost = host === req.host ? null : req.host;
  const address = hostWithPort(host, req.port);
  const shown = requestTarget(req.host, req.port);
  const wildcard = req.suggestedWildcard ? unicodeHost(req.suggestedWildcard) : null;
  const local = localOf(req);
  const savable = local && isSavable(local.listener) ? local.listener : null;
  const offersAlways = canSave(req) && (!local || savable !== null);

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
                May I reach {isLoopbackHost(req.host) ? shown : <code title={asciiHost ?? undefined}>{shown}</code>}?
                {local && <SpokenListener listener={local.listener} />}
                <span className={grantStyles.spokenReason}>{req.reason || '(no reason given)'}</span>
              </>,
            )
          ) : (
            <>
              <div className={styles.segmentText}>
                <code>{address}</code>
                {asciiHost && <span className={styles.mutedHint}>{asciiHost}</span>}
                <span className={styles.ruleChip}>{local ? 'not in allowed local ports' : 'not in allowed hosts'}</span>
              </div>
              <div className={grantStyles.reasonBlock}>
                <span className={grantStyles.reasonLabel}>Reason from agent:</span>
                <div className={grantStyles.reasonText}>{req.reason || '(no reason given)'}</div>
              </div>
              {local && <LocalListenerDetails listener={local.listener} port={local.port} />}
            </>
          )}
          <RunHosts hosts={req.runHosts} />
          <p className={styles.segmentHint}>
            <SaveScope req={req} workspaceName={workspaceName} />
          </p>
          {reasonShown && <DenyReasonField {...denyReason.field} disabled={isSubmitting} />}
          {resolved ? (
            <ResolvedRequestNotice onDismiss={onDecided && (() => onDecided(req.requestId))} />
          ) : (
            <div className={styles.segmentButtons}>
              <button
                type="button"
                className={`${styles.btn} ${styles.btnAllowOnce}`}
                onClick={() => send({ kind: 'allowRun' })}
                disabled={isSubmitting}
                {...chipText(
                  spoken,
                  'Allow this run',
                  'This run',
                  local ? allowRunTitle(local.listener, local.port) : 'A later run asks again.',
                )}
              />
              {offersAlways && (
                <button
                  type="button"
                  className={`${styles.btn} ${styles.btnAllow}`}
                  // A local grant is saved from the listener the card showed, never from this entry.
                  onClick={() => send({ kind: 'allowAlways', entry: local ? `localhost:${local.port}` : req.host })}
                  disabled={isSubmitting}
                  {...chipText(
                    spoken,
                    `Always for ${agentName}`,
                    'Always',
                    local && savable ? alwaysTitle(savable, local.port) : `Adds ${host} to this agent's allowed hosts.`,
                  )}
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
                className={`${styles.btn} ${styles.btnDenyOnce}`}
                onClick={() => send(denyReason.decision())}
                disabled={isSubmitting}
                aria-describedby={reasonShown ? denyReason.labelId : undefined}
                {...chipText(spoken, 'Deny', 'Deny')}
              />
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
      announce={(req) => `${requesterName(req)} wants network access to ${requestTarget(req.host, req.port)}.`}
      renderCard={(req, card) => <NetworkAccessCard {...card} req={req} requester={requester(req)} />}
    />
  );
};

export default InlineNetworkAccessCard;
