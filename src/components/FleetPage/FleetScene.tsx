import type { CSSProperties, ReactNode } from 'react';
import { useAppTheme } from '../../hooks/useAppTheme';
import AgentAvatar from '../Agents/AgentAvatar';
import {
  faceOf,
  identityRingColor,
  mainIdentity,
  type AgentActivity,
  type AgentIdentity,
} from '../Agents/agentIdentity';
import {
  shortTime,
  type FleetAgentRef,
  type FleetEarlierItem,
  type FleetResultTalk,
  type FleetRunningTalk,
  type FleetScene as Scene,
} from '../../fleet/fleetModel';
import styles from './FleetPage.module.css';

type Faces = Record<string, AgentIdentity>;

/** A hand-drawn speech-bubble tail; `right` points at the face on the right. */
const Tail = ({ right = false }: { right?: boolean }) => (
  <svg className={`${styles.tail} ${right ? styles.tailRight : ''}`} width="24" height="22" viewBox="0 0 28 24" aria-hidden="true">
    <path className={styles.tailFill} d="M28 3 C20 6 10 12 1 21 C11 17 19 15.5 28 15.5 Z" />
    <path className={styles.tailStroke} d="M25 3.6 C18 6.6 10 12 1 21 C11 17 18 15.6 25 15.5" />
  </svg>
);

/** A scalloped ellipse: eight arcs around the cloud's centre. */
const CLOUD = (() => {
  const points = Array.from({ length: 8 }, (_, i) => {
    const a = (i / 8) * Math.PI * 2 + 0.3;
    return [34 + Math.cos(a) * 29, 18 + Math.sin(a) * 13] as const;
  });
  let d = `M${points[0]![0].toFixed(1)} ${points[0]![1].toFixed(1)}`;
  for (let i = 1; i <= 8; i++) {
    const [x, y] = points[i % 8]!;
    const [px, py] = points[i - 1]!;
    const r = (Math.hypot(x - px, y - py) * 0.62).toFixed(1);
    d += ` A${r} ${r} 0 0 1 ${x.toFixed(1)} ${y.toFixed(1)}`;
  }
  return `${d}Z`;
})();

const ThoughtCloud = ({ name }: { name: string }) => (
  <svg className={styles.think} width="54" height="26" viewBox="0 0 96 40" role="img" aria-label={`${name} is working`}>
    <path d={CLOUD} className={styles.thinkCloud} />
    <circle cx="24" cy="18" r="3.2" className={styles.thinkDot} />
    <circle cx="34" cy="18" r="3.2" className={styles.thinkDot} />
    <circle cx="44" cy="18" r="3.2" className={styles.thinkDot} />
    <circle cx="74" cy="27" r="4.2" className={styles.thinkCloud} />
    <circle cx="86" cy="33" r="2.6" className={styles.thinkCloud} />
  </svg>
);

const Who = ({
  identity,
  name,
  size,
  activity = 'none',
  label,
  children,
}: {
  identity: AgentIdentity;
  name: string;
  size: number;
  activity?: AgentActivity;
  /** What the face is doing, for a tooltip. */
  label?: string;
  children?: ReactNode;
}) => (
  <figure className={styles.who} title={label}>
    <AgentAvatar identity={identity} size={size} activity={activity} ring={false} className={styles.face} />
    {children}
    <figcaption>{name}</figcaption>
  </figure>
);

const useAgentColor = (identity: AgentIdentity) => {
  const theme = useAppTheme();
  return { '--agent-color': identityRingColor(identity, theme) } as CSSProperties;
};

const Check = () => (
  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" aria-hidden="true">
    <path d="M5 12.5l4.5 4.5L19 7" />
  </svg>
);

const Cross = () => (
  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" aria-hidden="true">
    <path d="M6 6l12 12M18 6L6 18" />
  </svg>
);

const who = (t: FleetRunningTalk) => `${t.agent.name}${t.helpers ? ` +${t.helpers}` : ''}${t.queued ? ' · next' : ''}`;
const onIt = (t: FleetRunningTalk) => `${who(t)}${t.queued ? '' : ', on it'}`;

/** Main hands a task over (left); the agent thinks it through (right). */
const RunningTalk = ({ talk, compact, faces }: { talk: FleetRunningTalk; compact: boolean; faces: Faces }) => {
  const identity = faceOf(faces, talk.agent.seed);
  const size = compact ? 32 : 44;
  return (
    <div className={`${styles.talk} ${compact ? styles.talkCompact : ''}`} style={useAgentColor(identity)}>
      <Who identity={mainIdentity()} name="Main" size={size} />
      <div className={styles.say}>
        <div className={styles.bubble}>
          <Tail />
          <span className={styles.bubbleText} title={talk.title}>
            {talk.title}
          </span>
        </div>
      </div>
      <Who identity={identity} name={who(talk)} size={size} activity="running" label={onIt(talk)}>
        {!talk.queued && <ThoughtCloud name={talk.agent.name} />}
      </Who>
    </div>
  );
};

/** The task ended: the agent answers, and Main's question shrinks to one grey line. */
const ResultTalk = ({ talk, compact, now, faces }: { talk: FleetResultTalk; compact: boolean; now: number; faces: Faces }) => {
  const identity = faceOf(faces, talk.agent.seed);
  return (
    <div
      className={`${styles.talk} ${styles.result} ${talk.failed ? styles.resultFailed : ''} ${compact ? styles.talkCompact : ''}`}
      style={useAgentColor(identity)}
    >
      <Who identity={mainIdentity()} name="Main" size={compact ? 28 : 36} />
      <div className={styles.say}>
        <p className={styles.asked} title={talk.asked}>
          Main asked: {talk.asked}
        </p>
        <div className={styles.bubble}>
          <Tail right />
          <span className={styles.bubbleText} title={talk.result}>
            {talk.result}
          </span>
        </div>
        <span className={`${styles.meta} ${styles.metaEnd} ${talk.failed ? styles.metaBad : styles.metaOk}`}>
          {talk.failed ? <Cross /> : <Check />} {talk.failed ? 'failed' : 'done'} · {shortTime(talk.at, now)}
        </span>
      </div>
      <Who identity={identity} name={talk.agent.name} size={compact ? 36 : 48} />
    </div>
  );
};

/** Main's last word, addressed to the reader; smaller and greyer under live work. */
const ToYou = ({ scene, quiet, now }: { scene: Scene; quiet: boolean; now: number }) => {
  const asleep = scene.mood === 'idle' || scene.mood === 'stale';
  const note = scene.toYou;
  return (
    <div className={`${styles.toYou} ${quiet ? styles.toYouQuiet : ''}`}>
      <Who identity={mainIdentity()} name="Main" size={quiet ? 32 : 48} activity={asleep ? 'idle' : 'none'}>
        {asleep && (
          <span className={styles.zz} aria-hidden="true">
            zz
          </span>
        )}
        {scene.mainBusy && !quiet && <ThoughtCloud name="Main" />}
      </Who>
      {note ? (
        <div className={styles.say}>
          <div className={styles.bubble}>
            <Tail />
            <span className={styles.bubbleText} title={note.plain}>
              {quiet ? note.headline : note.plain}
            </span>
          </div>
          <span className={styles.meta}>to you · {shortTime(note.at, now)}</span>
        </div>
      ) : (
        <span className={`${styles.meta} ${styles.metaRun}`}>Main is on it</span>
      )}
    </div>
  );
};

const OUTCOME_LABEL: Record<FleetEarlierItem['outcome'], string> = {
  done: '',
  answered: 'answered',
  stopped: 'stopped',
  failed: 'failed',
};

const EarlierItem = ({ item, now, faces }: { item: FleetEarlierItem; now: number; faces: Faces }) => {
  const agent: FleetAgentRef = item.agent ?? { name: 'You', seed: '' };
  const label = OUTCOME_LABEL[item.outcome];
  return (
    <li className={styles.earlierItem}>
      {item.agent && (
        <AgentAvatar identity={faceOf(faces, agent.seed)} size={16} ring={false} className={styles.earlierFace} />
      )}
      <span className={styles.earlierText} title={item.title}>
        <b>{agent.name}</b> {item.title}
      </span>
      <span className={`${styles.earlierStatus} ${item.outcome === 'failed' ? styles.earlierFailed : ''}`}>
        {item.outcome === 'failed' ? <Cross /> : item.outcome === 'stopped' ? null : <Check />}
        {label ? `${label} · ` : ''}
        {shortTime(item.at, now)}
      </span>
    </li>
  );
};

/**
 * A small card as a comic panel: the caption says when, the bubbles say
 * what. Rows are in priority order; the ones that don't fit drop whole.
 */
const FleetScene = ({ scene, now, faces }: { scene: Scene; now: number; faces: Faces }) => {
  const talks = scene.running.length + scene.finished.length;
  const compact = talks > 1;
  const showMain = scene.toYou || (scene.mainBusy && talks === 0);
  return (
    <div className={styles.scene} data-mood={scene.mood}>
      <p className={styles.captionRow}>
        <span className={styles.caption} data-mood={scene.mood}>
          {scene.caption}
        </span>
      </p>
      {scene.running.map((talk) => (
        <RunningTalk key={talk.key} talk={talk} compact={compact} faces={faces} />
      ))}
      {scene.finished.map((talk) => (
        <ResultTalk key={talk.key} talk={talk} compact={compact} now={now} faces={faces} />
      ))}
      {showMain && <ToYou scene={scene} quiet={talks > 0} now={now} />}
      {scene.userMessage && (
        <div className={styles.fromYou}>
          <div className={styles.bubble}>
            <Tail right />
            <span className={styles.bubbleText} title={scene.userMessage.text}>
              {scene.userMessage.text}
            </span>
          </div>
          <span className={styles.meta}>you · {shortTime(scene.userMessage.at, now)}</span>
        </div>
      )}
      {!showMain && talks === 0 && !scene.userMessage && <p className={styles.nothing}>No messages yet.</p>}
      {scene.earlier.length > 0 && (
        <ul className={styles.earlier} aria-label="Earlier">
          {scene.earlier.map((item) => (
            <EarlierItem key={item.key} item={item} now={now} faces={faces} />
          ))}
        </ul>
      )}
    </div>
  );
};

export default FleetScene;
