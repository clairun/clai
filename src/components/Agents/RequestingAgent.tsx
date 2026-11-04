import type { SenderAgent } from '../AssistantChat/messageSender';
import AgentAvatar from './AgentAvatar';
import styles from './RequestingAgent.module.css';

/** The face and name of the agent asking the user for something. */
const RequestingAgent = ({
  agent,
}: {
  agent: Pick<SenderAgent, 'name' | 'identity' | 'presence'>;
}) => (
  <span className={styles.requester}>
    <AgentAvatar
      identity={agent.identity}
      size={18}
      activity={agent.presence === 'disabled' ? 'disabled' : 'none'}
    />
    <span className={styles.name}>{agent.name}</span>
  </span>
);

export default RequestingAgent;
