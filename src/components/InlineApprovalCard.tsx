import { useState, type ReactNode, type Ref } from 'react';
import { listPendingPermissionRequests, submitPermissionDecision } from '../permissions/client';
import type {
  AskFirstDecision,
  AskFirstRow,
  PermissionRequest,
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
import styles from './InlineApprovalCard.module.css';

const PERMISSION_REQUEST_EVENT = 'permissions://request';
const PERMISSION_RESOLVED_EVENT = 'permissions://resolved';


/** The backend sends a row per (part, rule); a part matching several rules is still one part. */
const partsOf = (rows: AskFirstRow[]) => {
  const parts: { text: string; rules: string[] }[] = [];
  for (const { text, rule } of rows) {
    const part = parts.find((p) => p.text === text);
    if (!part) parts.push({ text, rules: [rule] });
    else if (!part.rules.includes(rule)) part.rules.push(rule);
  }
  return parts;
};

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
  const { send, isSubmitting, error, resolved } = useDecisionSender(
    (decision: AskFirstDecision) => submitPermissionDecision(req.requestId, decision),
    () => onDecided?.(req.requestId),
  );
  // Deny answers in one click, with whatever reason is typed. The Fleet's
  // card has no room for the field, so its Deny sends none.
  const denyReason = useDenyReason();
  // Collapse: squish the card to a one-line summary so a tall (or stacked)
  // request can't swallow the conversation. Default expanded.
  const [isCollapsed, setCollapsed] = useState(false);
  const spoken = Boolean(speaker);
  const reasonShown = !spoken && !resolved;

  const rules = [...new Set(req.rows.map((row) => row.rule))];
  const parts = partsOf(req.rows);
  const matchesLabel =
    parts.length > 1
      ? 'These parts of the command are on the ask-first list:'
      : parts[0]?.text.trim() === req.command.trim()
        ? 'This command is on the ask-first list:'
        : 'Part of this command is on the ask-first list:';
  const scope = alwaysScope(req);
  const reach = scope === 'run' ? 'this run' : scope === 'everywhere' ? 'every workspace' : (workspaceName ?? 'this workspace');


  return (
    <article
      ref={ref}
      tabIndex={-1}
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
            <p className={styles.segmentsLabel}>{matchesLabel}</p>
            <ul className={styles.matchList} aria-label="Ask-first matches">
              {parts.map((part) => (
                <li key={part.text} className={styles.segmentRow}>
                  <div className={styles.segmentText}>
                    <code title={spoken ? part.text : undefined}>{part.text}</code>
                    {part.rules.map((rule) => (
                      <span key={rule} className={styles.ruleChip} title={`Matches the ask-first rule “${rule}”`}>
                        {rule}
                      </span>
                    ))}
                  </div>
                </li>
              ))}
            </ul>
            <p className={styles.segmentHint}>
              <AlwaysAllowScope req={req} rules={rules} workspaceName={workspaceName} />
            </p>
            {reasonShown && <DenyReasonField {...denyReason.field} disabled={isSubmitting} />}
            {resolved ? (
              <ResolvedRequestNotice onDismiss={onDecided && (() => onDecided(req.requestId))} />
            ) : (
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
                  className={`${styles.btn} ${styles.btnDenyOnce}`}
                  onClick={() => send(denyReason.decision())}
                  disabled={isSubmitting}
                  aria-describedby={reasonShown ? denyReason.labelId : undefined}
                  {...chipText(spoken, 'Deny', 'Deny')}
                />
              </div>
            )}
          </section>
          {error && !isSubmitting && <div className={styles.error} role="alert">{error}</div>}
        </div>
      )}
    </article>
  );
};

const PERMISSION_REQUESTS: PendingRequestSource<PermissionRequest> = {
  list: listPendingPermissionRequests,
  requestEvent: PERMISSION_REQUEST_EVENT,
  resolvedEvent: PERMISSION_RESOLVED_EVENT,
  isRequest: (req) => Array.isArray(req.rows),
};

/**
 * The workspace's pending ask-first requests, each a card beside the chat
 * that produced it, so a request in workspace B never covers workspace A.
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
  const requester = (req: { agentId: string | null }) => requestingAgent(req.agentId, roster, loadedTasks?.() ?? []);
  const requesterName = (req: { agentId: string | null }) => requester(req)?.name ?? 'An agent';
  return (
    <PendingRequestStack
      workspaceId={workspaceId}
      source={PERMISSION_REQUESTS}
      className={styles.stack}
      announce={(req) => `${requesterName(req)} wants to run a command on your ask-first list.`}
      renderCard={(req, card) => <ApprovalCard {...card} req={req} requester={requester(req)} />}
    />
  );
};

export default InlineApprovalCard;
