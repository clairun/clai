/** Who speaks in a chat: the session's owner, the user, another agent, or a task. */
import type {
  AgentRefSnapshot,
  AssistantMessage,
  TaskStatus,
  WorkspaceAgentResponse,
  WorkspaceTaskResponse,
} from '../../generated/bindings';
import { agentDisplayName, identityFor, type AgentIdentity } from '../Agents/agentIdentity';

export type AgentPresence = 'active' | 'disabled' | 'removed';

export interface SenderAgent {
  workspaceAgentId: string;
  name: string;
  identity: AgentIdentity;
  presence: AgentPresence;
}

/** The agent answering as `assistant` in a session; `null` when no workspace agent owns it. */
export type SessionOwner = AgentRefSnapshot | null;

const snapshotOf = (agent: WorkspaceAgentResponse): AgentRefSnapshot => ({
  workspaceAgentId: agent.id,
  agentDefinitionId: agent.agentDefinitionId,
  displayName: agent.displayName,
  isDefault: agent.isDefault,
  avatar: agent.avatar,
});

/** A task's log belongs to its assignee; the workspace chat to the Main. */
export const resolveSessionOwner = (
  roster: readonly WorkspaceAgentResponse[],
  task?: Pick<
    WorkspaceTaskResponse,
    'assignedToWorkspaceAgentId' | 'assignedAgentDefinitionId' | 'assignedAgentDisplayName'
  > | null
): SessionOwner => {
  if (task) {
    return {
      workspaceAgentId: task.assignedToWorkspaceAgentId,
      agentDefinitionId: task.assignedAgentDefinitionId,
      displayName: task.assignedAgentDisplayName,
      isDefault: false,
    };
  }
  const main = roster.find((agent) => agent.isDefault);
  return main ? snapshotOf(main) : null;
};

/**
 * The live roster wins, so renames and face changes apply to old messages; an
 * agent that left the crew keeps the snapshot's name and face.
 */
export const resolveAgent = (
  snapshot: AgentRefSnapshot,
  roster: readonly WorkspaceAgentResponse[]
): SenderAgent => {
  const live = roster.find((agent) => agent.id === snapshot.workspaceAgentId);
  if (live) {
    return {
      workspaceAgentId: live.id,
      name: agentDisplayName(live),
      identity: identityFor(live),
      presence: live.enabled ? 'active' : 'disabled',
    };
  }
  return {
    workspaceAgentId: snapshot.workspaceAgentId,
    name: agentDisplayName(snapshot),
    identity: identityFor({ ...snapshot, id: snapshot.workspaceAgentId }),
    presence: 'removed',
  };
};

/**
 * Who wrote a message. Only `user` renders in the user's style; anything the
 * user did not type is an agent, a task, or (for an origin this build does not
 * know) "System".
 */
export type MessageSender =
  | { kind: 'user' }
  | { kind: 'owner'; agent: SenderAgent | null }
  | { kind: 'agent'; agent: SenderAgent }
  | {
      kind: 'task';
      agent: SenderAgent;
      taskId: string;
      title: string;
      status: TaskStatus;
      cycleNo: number;
    }
  | { kind: 'unknown' };

export const resolveMessageSender = (
  message: AssistantMessage,
  owner: SenderAgent | null,
  roster: readonly WorkspaceAgentResponse[]
): MessageSender => {
  if (message.role === 'assistant') return { kind: 'owner', agent: owner };
  // Rows written before origins existed carry none: they were the user's.
  const origin = message.origin ?? { kind: 'user' };
  switch (origin.kind) {
    case 'user':
      return { kind: 'user' };
    case 'agent':
      return { kind: 'agent', agent: resolveAgent(origin.agent, roster) };
    case 'task':
      return {
        kind: 'task',
        agent: resolveAgent(origin.agent, roster),
        taskId: origin.taskId,
        title: origin.title,
        status: origin.status,
        cycleNo: Number(origin.cycleNo),
      };
    default:
      return { kind: 'unknown' };
  }
};

/**
 * Consecutive blocks by the same agent merge under one header. The user's
 * messages and task cards always stand alone.
 */
export const senderGroupKey = (sender: MessageSender): string | null => {
  if (sender.kind === 'owner') return 'owner';
  if (sender.kind === 'agent') return `agent:${sender.agent.workspaceAgentId}`;
  return null;
};

/** Where a task's question stands, read from the live task: still open, or how it closed. */
export type QuestionState = { open: true } | { open: false; label: string };

const ENDED_WITHOUT_ANSWER = new Set(['stopping', 'stopped', 'failed']);

/** `null` when the live task is unknown, so the card claims nothing. */
export const questionState = (
  sender: Extract<MessageSender, { kind: 'task' }>,
  liveTask: WorkspaceTaskResponse | null,
  roster: readonly WorkspaceAgentResponse[]
): QuestionState | null => {
  if (!liveTask) return null;
  const liveCycle = Number(liveTask.cycleNo);
  if (liveCycle > sender.cycleNo) return { open: false, label: 'Answered' };
  if (liveTask.status === 'input_required') return { open: true };
  const answeredBy = liveTask.answeredBy;
  if (answeredBy?.kind === 'user') return { open: false, label: 'Answered by you' };
  if (answeredBy?.kind === 'agent' || answeredBy?.kind === 'task') {
    return { open: false, label: `Answered by ${resolveAgent(answeredBy.agent, roster).name}` };
  }
  if (!answeredBy && ENDED_WITHOUT_ANSWER.has(liveTask.status)) {
    return { open: false, label: 'Not answered · task stopped' };
  }
  return { open: false, label: 'Answered' };
};
