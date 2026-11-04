import type { SenderAgent } from '../AssistantChat/messageSender';
import AgentAvatar from './AgentAvatar';
import styles from './RequestingAgent.module.css';

/**
 * The face and name of the agent asking the user for something. The face
 * carries the accessible name, so the printed name is not read twice.
 */
const RequestingAgent = ({
  agent,
}: {
  agent: Pick<SenderAgent, 'name' | 'identity' | 'presence'>;
}) => (
  <span className={styles.requester}>
    <AgentAvatar
      identity={agent.identity}
      size={18}
      label={agent.name}
      activity={agent.presence === 'disabled' ? 'disabled' : 'none'}
    />
    <span className={styles.name} aria-hidden="true">
      {agent.name}
    </span>
  </span>
);

export default RequestingAgent;
