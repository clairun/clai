import type { CSSProperties, MouseEvent } from 'react';
import { useAppTheme } from '../../hooks/useAppTheme';
import AgentAvatar from '../Agents/AgentAvatar';
import { faceOf, identityRingColor, mainIdentity, type AgentIdentity } from '../Agents/agentIdentity';
import AskUserPanel from '../AskUserPanel/AskUserPanel';
import InlineApprovalCard from '../InlineApprovalCard';
import InlinePathGrantCard from '../InlinePathGrantCard';
import { agentOf, type FleetLive, type FleetRequest } from '../../fleet/fleetModel';
import styles from './FleetPage.module.css';

const MAIN = { name: 'Main', identity: mainIdentity() };

/** Clicks inside the request answer it; they never select the card. */
const stop = (e: MouseEvent) => e.stopPropagation();

/** Who is asking: a question's session is a task's, or the main's; approvals carry the agent. */
const asker = (request: FleetRequest, workspaceId: string, live: FleetLive, faces: Record<string, AgentIdentity>) => {
  if (request.kind === 'ask') {
    const task = Object.values(live.tasks[workspaceId] ?? {}).find((t) => t.sessionId === request.ask.sessionId);
    if (!task) return MAIN;
    const { name, seed } = agentOf(task);
    return { name, identity: faceOf(faces, seed) };
  }
  return request.agentName ? { name: request.agentName, identity: faceOf(faces, request.agentName) } : MAIN;
};

/** A request answered with the same UI as the workspace chat. */
const RequestAnswer = ({ request, workspaceId }: { request: FleetRequest; workspaceId: string }) =>
  request.kind === 'ask' ? (
    <AskUserPanel key={request.ask.pendingId} ask={request.ask} />
  ) : request.kind === 'approval' ? (
    <InlineApprovalCard key={request.request.requestId} workspaceId={workspaceId} request={request.request} />
  ) : (
    <InlinePathGrantCard key={request.request.requestId} workspaceId={workspaceId} request={request.request} />
  );

interface FleetRequestRowProps {
  /** The card's open requests, oldest first; the first is answered here, the next once it resolves. */
  requests: FleetRequest[];
  workspaceId: string;
  live: FleetLive;
  faces: Record<string, AgentIdentity>;
}

/** A small card's open request, in full and answerable in place; it takes the card's body. */
const FleetRequestRow = ({ requests, workspaceId, live, faces }: FleetRequestRowProps) => {
  const theme = useAppTheme();
  const request = requests[0]!;
  const { name, identity } = asker(request, workspaceId, live, faces);
  return (
    <section
      className={styles.request}
      style={{ '--agent-color': identityRingColor(identity, theme) } as CSSProperties}
      aria-label="Needs you"
      onClick={stop}
    >
      <div className={styles.requestHead}>
        <AgentAvatar identity={identity} size={28} ring={false} label={name} />
        <span className={styles.liveAgent}>{name}</span>
        {requests.length > 1 && <span className={styles.requestCount}>1 of {requests.length}</span>}
      </div>
      <RequestAnswer request={request} workspaceId={workspaceId} />
    </section>
  );
};

export default FleetRequestRow;
