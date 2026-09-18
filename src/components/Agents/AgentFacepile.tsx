/**
 * A few overlapping faces standing in for "N agents" — the header chip. The
 * Main comes first, then whoever is working, so the faces that matter are
 * the ones that fit. Rings only for running/attention: an idle hairline on
 * every face would turn the chip into a row of targets.
 */
import type { WorkspaceAgentResponse, WorkspaceTaskResponse } from '../../generated/bindings';
import AgentAvatar from './AgentAvatar';
import { agentActivity, identityFor, type AgentActivity } from './agentIdentity';
import styles from './AgentFacepile.module.css';

export interface AgentFacepileProps {
  agents: readonly WorkspaceAgentResponse[];
  tasks: readonly WorkspaceTaskResponse[];
  /** How many faces before the "+N" bubble. Default 4. */
  max?: number;
  /** CSS px per face. Default 18. */
  size?: number;
}

const RANK: Record<AgentActivity, number> = {
  running: 0,
  attention: 1,
  idle: 2,
  none: 2,
  disabled: 3,
};

/** Faces in the order they should compete for the visible slots. */
export const facepileOrder = (
  agents: readonly WorkspaceAgentResponse[],
  tasks: readonly WorkspaceTaskResponse[]
): { agent: WorkspaceAgentResponse; activity: AgentActivity }[] =>
  agents
    .map((agent) => ({ agent, activity: agentActivity(agent, tasks) }))
    .sort((a, b) => {
      if (a.agent.isDefault !== b.agent.isDefault) return a.agent.isDefault ? -1 : 1;
      return RANK[a.activity] - RANK[b.activity];
    });

const AgentFacepile = ({ agents, tasks, max = 4, size = 18 }: AgentFacepileProps) => {
  const ordered = facepileOrder(agents, tasks);
  const shown = ordered.slice(0, max);
  const rest = ordered.length - shown.length;
  return (
    <span className={styles.pile} style={{ '--facepile-size': `${size}px` } as React.CSSProperties}>
      {shown.map(({ agent, activity }) => (
        <AgentAvatar
          key={agent.id}
          identity={identityFor(agent)}
          size={size}
          // Quiet unless something is happening.
          activity={activity === 'running' || activity === 'attention' ? activity : 'none'}
          className={styles.face}
        />
      ))}
      {rest > 0 && <span className={styles.more}>+{rest}</span>}
    </span>
  );
};

export default AgentFacepile;
