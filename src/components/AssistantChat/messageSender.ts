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

/** A workspace agent by its id, as the roster draws it; `null` when the roster does not know it. */
export const rosterAgent = (
  workspaceAgentId: string | null | undefined,
  roster: readonly WorkspaceAgentResponse[]
): SenderAgent | null => {
  const live = workspaceAgentId ? roster.find((agent) => agent.id === workspaceAgentId) : undefined;
  if (!live) return null;
  return {
    workspaceAgentId: live.id,
    name: agentDisplayName(live),
    identity: identityFor(live),
    presence: live.enabled ? 'active' : 'disabled',
  };
};

/**
 * The live roster wins, so renames and face changes apply to old messages; an
 * agent that left the crew keeps the snapshot's name and face.
 */
export const resolveAgent = (
  snapshot: AgentRefSnapshot,
  roster: readonly WorkspaceAgentResponse[]
): SenderAgent => {
  const live = rosterAgent(snapshot.workspaceAgentId, roster);
  if (live) return live;
  return {
    workspaceAgentId: snapshot.workspaceAgentId,
    name: agentDisplayName(snapshot),
    identity: identityFor({ ...snapshot, id: snapshot.workspaceAgentId }),
    presence: 'removed',
  };
};

/**
 * The agent that raised an approval or a grant: found in the roster or, for an
 * agent removed mid-task, on its task, as that task's log draws it.
 */
export const requestingAgent = (
  workspaceAgentId: string | null | undefined,
  roster: readonly WorkspaceAgentResponse[],
  tasks: readonly WorkspaceTaskResponse[]
): SenderAgent | null => {
  const live = rosterAgent(workspaceAgentId, roster);
  if (live || !workspaceAgentId) return live;
  const task = tasks.find((entry) => entry.assignedToWorkspaceAgentId === workspaceAgentId);
  const owner = task ? resolveSessionOwner(roster, task) : null;
  return owner ? resolveAgent(owner, roster) : null;
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
