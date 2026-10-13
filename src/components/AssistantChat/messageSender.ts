/** Who speaks in a chat: the agent behind a session's `assistant` messages. */
import type {
  AgentRefSnapshot,
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
