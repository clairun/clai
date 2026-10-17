import { useState, type CSSProperties, type KeyboardEvent, type MouseEvent } from 'react';
import { useAppTheme } from '../../hooks/useAppTheme';
import AgentAvatar from '../Agents/AgentAvatar';
import { GENERATOR_VERSION, identityRingColor, mainIdentity, type AgentIdentity } from '../Agents/agentIdentity';
import pillStyles from '../Agents/taskStatus.module.css';
import { OTHER_INDEX, useAskAnswer } from '../AskUserPanel/useAskAnswer';
import { allowOnce, denyOnce, submitPermissionDecision } from '../../permissions/client';
import {
  allowAlwaysDecision,
  allowOnceDecision,
  denyDecision,
  submitPathGrantDecision,
} from '../../permissions/pathGrantsClient';
import type { FleetAsk, FleetLive, FleetRequest } from '../../fleet/fleetModel';
import type { PathGrantRequest, PermissionRequest } from '../../generated/bindings';
import styles from './FleetPage.module.css';

const MAIN = { name: 'Main', identity: mainIdentity() };

/** Clicks on the answer controls answer; they don't select the card. */
const stop = (e: MouseEvent) => e.stopPropagation();

const errorText = (err: unknown): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to submit decision';

/** One decision in flight; the row goes away on the backend's `resolved`, not here. */
function useDecision() {
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const decide = async (send: () => Promise<void>) => {
    if (submitting) return;
    setSubmitting(true);
    setError('');
    try {
      await send();
    } catch (err) {
      setError(errorText(err));
      setSubmitting(false);
    }
  };
  return { submitting, error, decide };
}

const AskControls = ({ ask }: { ask: FleetAsk }) => {
  const answer = useAskAnswer(ask);
  const [typing, setTyping] = useState(false);
  const showInput = !answer.hasOptions || typing;
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key !== 'Enter') return;
    e.preventDefault();
    void answer.submit();
  };
  return (
    <>
      {answer.hasOptions && (
        <div className={styles.requestButtons}>
          {answer.options.map((option, index) => (
            <button
              key={index}
              type="button"
              className={styles.requestButton}
              title={option.description ?? undefined}
              aria-pressed={answer.isMultiSelect ? answer.selectedIndexes.has(index) : undefined}
              disabled={answer.submitting}
              onClick={() => {
                if (!answer.isMultiSelect) return void answer.submitOption(index);
                answer.setSelectedIndexes((current) => {
                  const next = new Set(current);
                  if (!next.delete(index)) next.add(index);
                  return next;
                });
              }}
            >
              {option.label}
            </button>
          ))}
          {!typing && (
            <button type="button" className={styles.requestButton} onClick={() => setTyping(true)} disabled={answer.submitting}>
              Other…
            </button>
          )}
          {answer.isMultiSelect && (
            <button type="button" className={styles.requestPrimary} onClick={() => void answer.submit()} disabled={!answer.canSubmit}>
              Send
            </button>
          )}
        </div>
      )}
      {showInput && (
        <input
          className={styles.requestInput}
          aria-label="Your answer"
          placeholder="Type an answer, Enter sends"
          value={answer.otherText}
          autoFocus={typing}
          disabled={answer.submitting}
          onChange={(e) => {
            answer.setOtherText(e.target.value);
            if (answer.isMultiSelect) answer.setOtherChecked(e.target.value.trim().length > 0);
            else if (answer.hasOptions) answer.setSelectedIndex(OTHER_INDEX);
          }}
          onKeyDown={onKeyDown}
        />
      )}
      {answer.error && <p className={styles.requestError}>{answer.error}</p>}
    </>
  );
};

const ApprovalControls = ({ request }: { request: PermissionRequest }) => {
  const { submitting, error, decide } = useDecision();
  const send = (decision: typeof allowOnce) => () =>
    decide(() => submitPermissionDecision(request.requestId, request.segments.map(decision)));
  return (
    <>
      <div className={styles.requestButtons}>
        <button type="button" className={styles.requestPrimary} onClick={send(allowOnce)} disabled={submitting}>
          Approve
        </button>
        <button type="button" className={styles.requestButton} onClick={send(denyOnce)} disabled={submitting}>
          Deny
        </button>
      </div>
      {error && <p className={styles.requestError}>{error}</p>}
    </>
  );
};

const GrantControls = ({ request }: { request: PathGrantRequest }) => {
  const { submitting, error, decide } = useDecision();
  const { requestId, requestedPath: path, requestedAccess: access } = request;
  return (
    <>
      <div className={styles.requestButtons}>
        <button
          type="button"
          className={styles.requestPrimary}
          onClick={() => decide(() => submitPathGrantDecision(requestId, allowOnceDecision(path, access)))}
          disabled={submitting}
        >
          Allow once
        </button>
        <button
          type="button"
          className={styles.requestButton}
          onClick={() => decide(() => submitPathGrantDecision(requestId, allowAlwaysDecision(path, access)))}
          disabled={submitting}
        >
          Always allow
        </button>
        <button
          type="button"
          className={styles.requestButton}
          onClick={() => decide(() => submitPathGrantDecision(requestId, denyDecision()))}
          disabled={submitting}
        >
          Deny
        </button>
      </div>
      {error && <p className={styles.requestError}>{error}</p>}
    </>
  );
};

/** Who is asking: a question's session is a task's, or the main's; approvals carry the agent. */
const asker = (request: FleetRequest, workspaceId: string, live: FleetLive, faces: Record<string, AgentIdentity>) => {
  if (request.kind === 'ask') {
    const task = Object.values(live.tasks[workspaceId] ?? {}).find((t) => t.sessionId === request.ask.sessionId);
    if (!task) return MAIN;
    const seed = task.assignedAgentDefinitionId || task.assignedToWorkspaceAgentId;
    return { name: task.assignedAgentDisplayName, identity: faces[seed] ?? { seed, generatorVersion: GENERATOR_VERSION } };
  }
  const seed = request.agentName;
  if (!seed) return MAIN;
  return { name: seed, identity: faces[seed] ?? { seed, generatorVersion: GENERATOR_VERSION } };
};

interface FleetRequestRowProps {
  /** The card's open requests, newest first; the first is answered here. */
  requests: FleetRequest[];
  workspaceId: string;
  live: FleetLive;
  faces: Record<string, AgentIdentity>;
}

/** A small card's open request, answerable in place, in the live-row style. */
const FleetRequestRow = ({ requests, workspaceId, live, faces }: FleetRequestRowProps) => {
  const theme = useAppTheme();
  const request = requests[0]!;
  const { name, identity } = asker(request, workspaceId, live, faces);
  return (
    <section
      className={styles.request}
      style={{ '--agent-color': identityRingColor(identity, theme) } as CSSProperties}
      aria-label="Needs you"
    >
      <div className={styles.requestHead}>
        <AgentAvatar identity={identity} size={28} ring={false} label={name} />
        <span className={styles.liveAgent}>{name}</span>
        <span className={`${pillStyles.pill} ${pillStyles.pill_blocked}`}>Needs you</span>
      </div>
      <p className={styles.requestText} title={request.text}>
        {request.text}
      </p>
      <div className={styles.requestControls} onClick={stop}>
        {request.kind === 'ask' ? (
          <AskControls key={request.ask.pendingId} ask={request.ask} />
        ) : request.kind === 'approval' ? (
          <ApprovalControls key={request.request.requestId} request={request.request} />
        ) : (
          <GrantControls key={request.request.requestId} request={request.request} />
        )}
      </div>
      {requests.length > 1 && (
        <button type="button" className={styles.requestMore}>
          +{requests.length - 1} more
        </button>
      )}
    </section>
  );
};

export default FleetRequestRow;
