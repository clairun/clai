import type { CSSProperties, MouseEvent, ReactNode } from 'react';
import { useAppTheme } from '../../hooks/useAppTheme';
import {
  faceOf,
  identityRingColor,
  mainIdentity,
  type AgentIdentity,
} from '../Agents/agentIdentity';
import AskUserPanel from '../AskUserPanel/AskUserPanel';
import InlineApprovalCard from '../InlineApprovalCard';
import InlinePathGrantCard from '../InlinePathGrantCard';
import type { FleetRequest, FleetRequestScene } from '../../fleet/fleetModel';
import fx, { Sfx } from './FleetEffects';
import { Caption, Tail, Who } from './FleetScene';
import styles from './FleetPage.module.css';

/** Clicks inside the request answer it; they never select the card. */
const stop = (e: MouseEvent) => e.stopPropagation();

/** The request, answered with the workspace chat's own UI; `speaker` draws its words. */
const RequestAnswer = ({
  request,
  workspaceId,
  compact,
  speaker,
}: {
  request: FleetRequest;
  workspaceId: string;
  compact: boolean;
  speaker: (prompt: ReactNode) => ReactNode;
}) =>
  request.kind === 'ask' ? (
    <AskUserPanel
      key={request.ask.pendingId}
      ask={request.ask}
      compact={compact}
      speaker={speaker}
    />
  ) : request.kind === 'approval' ? (
    <InlineApprovalCard
      key={request.request.requestId}
      workspaceId={workspaceId}
      request={request.request}
      compact={compact}
      speaker={speaker}
    />
  ) : (
    <InlinePathGrantCard
      key={request.request.requestId}
      workspaceId={workspaceId}
      request={request.request}
      compact={compact}
      speaker={speaker}
    />
  );

interface FleetRequestPanelProps {
  scene: FleetRequestScene;
  workspaceId: string;
  faces: Record<string, AgentIdentity>;
  /** The stage: bigger, with every decision the workspace chat offers. */
  stage?: boolean;
  /** Draw it loud: the amber burst, and "!?" or "!!". Never on the selected card. */
  loud?: boolean;
  /** The time of the last Fleet event. */
  now: number;
}

/**
 * A card that needs the user, as the agent asking: its face, the question,
 * command or path as its bubble, and the answer below. It takes the whole
 * card, which the panel scrolls if the request is taller.
 */
const FleetRequestPanel = ({
  scene,
  workspaceId,
  faces,
  stage = false,
  loud = false,
  now,
}: FleetRequestPanelProps) => {
  const theme = useAppTheme();
  const { request, asker, count, caption } = scene;
  const identity = asker ? faceOf(faces, asker.seed) : mainIdentity();
  const name = asker?.name ?? 'Main';
  const speaker = (prompt: ReactNode) => (
    <div className={styles.asks}>
      <Who
        identity={identity}
        name={name}
        size={stage ? 64 : 44}
        activity="running"
        expression="asking"
      />
      <div className={styles.bubble}>
        <Tail />
        <div className={styles.askText}>{prompt}</div>
      </div>
    </div>
  );
  return (
    <div className={`${styles.requestFrame} ${fx.burstFrame}`} data-loud-frame={loud || undefined}>
      {loud && (
        <span className={fx.burst} data-size={stage ? 'stage' : 'small'} aria-hidden="true" />
      )}
      <section
        className={`${styles.request} ${stage ? styles.requestStage : ''}`}
        style={{ '--agent-color': identityRingColor(identity, theme) } as CSSProperties}
        aria-label={`${name} needs you`}
        onClick={stop}
      >
        <p className={styles.captionRow}>
          <Caption caption={caption} mood="waiting" now={now} />
          {count > 1 && <span className={styles.requestCount}>1 of {count}</span>}
          {loud && <Sfx word={request.kind === 'ask' ? '!?' : '!!'} tone="warn" />}
        </p>
        <RequestAnswer
          request={request}
          workspaceId={workspaceId}
          compact={!stage}
          speaker={speaker}
        />
      </section>
    </div>
  );
};

export default FleetRequestPanel;
