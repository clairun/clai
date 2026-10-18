import { useLayoutEffect, useRef, type CSSProperties, type ReactNode } from 'react';
import { useAppTheme } from '../../hooks/useAppTheme';
import AgentAvatar from '../Agents/AgentAvatar';
import MarkdownMessage from '../Chat/MarkdownMessage';
import {
  faceOf,
  identityRingColor,
  mainIdentity,
  type AgentActivity,
  type AgentIdentity,
} from '../Agents/agentIdentity';
import {
  captionText,
  shortTime,
  shoutLevel,
  type FleetAgentRef,
  type FleetCaption,
  type FleetEarlierItem,
  type FleetResultTalk,
  type FleetRunningTalk,
  type FleetScene as Scene,
} from '../../fleet/fleetModel';
import fx, { FaceExpression, Sfx, SpeedLines, type Expression } from './FleetEffects';
import styles from './FleetPage.module.css';

type Faces = Record<string, AgentIdentity>;

/** What only the stage has: everything in full, and tasks that open. */
export interface StageExtras {
  onOpenTask: (taskId: string) => void;
  /** Main's last message in full, as markdown (the card holds a clipped copy). */
  message: string | null;
  /** What Main is writing right now. */
  streaming: string | null;
}

/** A task title that opens the task's log on the stage, and is plain text on a small card. */
const TaskLink = ({ taskId, stage, children }: { taskId: string; stage?: StageExtras; children: ReactNode }) =>
  stage ? (
    <button type="button" className={styles.taskLink} onClick={() => stage.onOpenTask(taskId)} title="Open the task's log">
      {children}
    </button>
  ) : (
    <>{children}</>
  );

/** The caption box: when, worded against the page's clock. */
export const Caption = ({ caption, mood, now }: { caption: FleetCaption; mood: string; now: number }) => (
  <span className={styles.caption} data-mood={mood}>
    {captionText(caption, now)}
  </span>
);

/** A hand-drawn speech-bubble tail; `right` points at the face on the right. */
export const Tail = ({ right = false }: { right?: boolean }) => (
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

export const Who = ({
  identity,
  name,
  size,
  activity = 'none',
  label,
  expression,
  ghosts = false,
  children,
}: {
  identity: AgentIdentity;
  name: string;
  size: number;
  activity?: AgentActivity;
  /** What the face is doing, for a tooltip. */
  label?: string;
  /** A reaction drawn over the face. */
  expression?: Expression;
  /** Afterimages: the face rushing in. */
  ghosts?: boolean;
  children?: ReactNode;
}) => (
  <figure className={styles.who} title={label}>
    <span className={`${fx.actor} ${styles.face}`} style={{ width: size, height: size }}>
      {ghosts &&
        [0, 1].map((i) => (
          <span key={i} className={fx.ghost} aria-hidden="true">
            <AgentAvatar identity={identity} size={size} activity={activity} ring={false} />
          </span>
        ))}
      <AgentAvatar identity={identity} size={size} activity={activity} ring={false} />
      {expression && <FaceExpression kind={expression} size={size} />}
    </span>
    {children}
    <figcaption>{name}</figcaption>
  </figure>
);

const useAgentColor = (identity: AgentIdentity) => {
  const theme = useAppTheme();
  return { '--agent-color': identityRingColor(identity, theme) } as CSSProperties;
};

/** A tick; with a `label` it is the only word for the outcome, so it says it. */
const Check = ({ label }: { label?: string }) => (
  <svg
    width="11"
    height="11"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="3"
    {...(label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': true })}
  >
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

/**
 * Main hands a task over (left); the agent thinks it through (right). Its
 * panel tilts, alternating with the one above it; the newest task on the
 * page (`rush`) also rushes in: speed lines, afterimages and a "GO!".
 */
const RunningTalk = ({
  talk,
  index,
  compact,
  rush,
  now,
  faces,
  stage,
}: {
  talk: FleetRunningTalk;
  index: number;
  compact: boolean;
  rush: boolean;
  now: number;
  faces: Faces;
  stage?: StageExtras;
}) => {
  const identity = faceOf(faces, talk.agent.seed);
  const size = stage ? (compact ? 44 : 56) : compact ? 32 : 44;
  const live = !talk.queued;
  return (
    <div
      data-row
      className={`${styles.talk} ${compact ? styles.talkCompact : ''} ${live ? fx.tilt : ''}`}
      data-tilt={index % 2 ? 'right' : 'left'}
      style={useAgentColor(identity)}
    >
      {rush && <SpeedLines />}
      {rush && <Sfx word={index % 2 ? 'GO GO!' : 'GO!'} tone="go" level={shoutLevel(talk.at, now)} />}
      <Who identity={mainIdentity()} name="Main" size={size} />
      <div className={styles.say}>
        <div className={styles.bubble}>
          <Tail />
          <span className={styles.bubbleText} title={stage ? undefined : talk.title}>
            <TaskLink taskId={talk.taskId} stage={stage}>
              {talk.title}
            </TaskLink>
          </span>
        </div>
      </div>
      <Who identity={identity} name={who(talk)} size={size} activity="running" label={onIt(talk)} expression={live ? 'working' : undefined} ghosts={rush}>
        {!talk.queued && <ThoughtCloud name={talk.agent.name} />}
      </Who>
    </div>
  );
};

/** The task ended: the agent answers, and Main's question shrinks to one grey line. */
const ResultTalk = ({
  talk,
  compact,
  shout,
  now,
  faces,
  stage,
}: {
  talk: FleetResultTalk;
  compact: boolean;
  /** The page's one cheer: "DONE!". */
  shout: boolean;
  now: number;
  faces: Faces;
  stage?: StageExtras;
}) => {
  const identity = faceOf(faces, talk.agent.seed);
  return (
    <div
      data-row
      className={`${styles.talk} ${styles.result} ${talk.failed ? styles.resultFailed : ''} ${compact ? styles.talkCompact : ''}`}
      style={useAgentColor(identity)}
    >
      {shout && <Sfx word="DONE!" tone="done" level={shoutLevel(talk.at, now)} />}
      <Who identity={mainIdentity()} name="Main" size={compact ? 28 : 36} />
      <div className={styles.say}>
        <p className={styles.asked} title={talk.asked}>
          Main asked:{' '}
          <TaskLink taskId={talk.taskId} stage={stage}>
            {talk.asked}
          </TaskLink>
        </p>
        <div className={styles.bubble}>
          <Tail right />
          {stage ? (
            <div className={styles.markdown}>
              <MarkdownMessage content={talk.result} />
            </div>
          ) : (
            <span className={styles.bubbleText} title={talk.result}>
              {talk.result}
            </span>
          )}
        </div>
        <span className={`${styles.meta} ${styles.metaEnd} ${talk.failed ? styles.metaBad : styles.metaOk}`}>
          {talk.failed ? <Cross /> : <Check />} {talk.failed ? 'failed' : 'done'} · {shortTime(talk.at, now)}
        </span>
      </div>
      <Who identity={identity} name={talk.agent.name} size={compact ? 36 : 48} expression={talk.failed ? undefined : 'done'} />
    </div>
  );
};

/** Main's last word, addressed to the reader; smaller and greyer under live work. */
const ToYou = ({ scene, quiet, now, stage }: { scene: Scene; quiet: boolean; now: number; stage?: StageExtras }) => {
  const asleep = scene.mood === 'idle' || scene.mood === 'stale';
  const note = scene.toYou;
  const full = stage?.streaming || stage?.message || note?.text;
  return (
    <div className={`${styles.toYou} ${quiet ? styles.toYouQuiet : ''}`} data-row>
      <Who identity={mainIdentity()} name="Main" size={stage ? 56 : quiet ? 32 : 48} activity={asleep ? 'idle' : 'none'} expression={asleep ? 'asleep' : undefined}>
        {scene.mainBusy && !quiet && <ThoughtCloud name="Main" />}
      </Who>
      {stage && full ? (
        <div className={styles.say}>
          <div className={styles.bubble}>
            <Tail />
            <div className={styles.markdown}>
              <MarkdownMessage content={full} isStreaming={!!stage.streaming} />
            </div>
          </div>
          <span className={styles.meta}>
            {stage.streaming ? 'to you · writing…' : note ? `to you · ${shortTime(note.at, now)}` : 'to you'}
          </span>
        </div>
      ) : note ? (
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

const EarlierItem = ({ item, now, faces, stage }: { item: FleetEarlierItem; now: number; faces: Faces; stage?: StageExtras }) => {
  const agent: FleetAgentRef = item.agent ?? { name: 'You', seed: '' };
  const label = OUTCOME_LABEL[item.outcome];
  return (
    <li className={styles.earlierItem} data-row>
      {item.agent && (
        <AgentAvatar identity={faceOf(faces, agent.seed)} size={16} ring={false} className={styles.earlierFace} />
      )}
      <span className={styles.earlierText} title={stage ? undefined : item.title}>
        <b>{agent.name}</b>{' '}
        {item.taskId ? (
          <TaskLink taskId={item.taskId} stage={stage}>
            {item.title}
          </TaskLink>
        ) : (
          item.title
        )}
      </span>
      <span className={`${styles.earlierStatus} ${item.outcome === 'failed' ? styles.earlierFailed : ''}`}>
        {item.outcome === 'failed' ? <Cross /> : item.outcome === 'stopped' ? null : <Check label={label ? undefined : 'done'} />}
        {label ? `${label} · ` : ''}
        {shortTime(item.at, now)}
      </span>
      {item.result && <span className={styles.earlierResult}>{item.result}</span>}
    </li>
  );
};

const nearBottom = (node: HTMLElement) => node.scrollHeight - node.scrollTop - node.clientHeight < 40;

/**
 * Keeps the newest row in view. A small card hides whole rows from the top,
 * oldest first, until the rest fit under the caption; the stage scrolls and,
 * like the chat, follows new content (`grown`) while the reader is at the bottom.
 */
const useNewestInView = (stage: boolean, scene: Scene, grown: string | null | undefined) => {
  const ref = useRef<HTMLDivElement | null>(null);
  const pinned = useRef(true);
  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return undefined;
    if (stage) {
      if (pinned.current) node.scrollTop = node.scrollHeight;
      return undefined;
    }
    const fit = () => {
      const rows = [...node.querySelectorAll<HTMLElement>('[data-row]')];
      rows.forEach((row) => (row.hidden = false));
      for (const row of rows) {
        if (node.scrollHeight <= node.clientHeight) break;
        row.hidden = true;
      }
    };
    fit();
    if (typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver(fit);
    observer.observe(node);
    return () => observer.disconnect();
  }, [stage, scene, grown]);
  const onScroll = stage ? () => (pinned.current = !!ref.current && nearBottom(ref.current)) : undefined;
  return { ref, onScroll };
};

/**
 * A card as a comic panel, read top to bottom in the order things happened,
 * as the chat is: the caption says when, then Earlier, the results, Main's
 * word to you and, last, what runs now. Which rows a card has room for is
 * `cardScene`'s call; this only orders them, oldest first.
 */
const FleetScene = ({
  scene,
  shouts,
  now,
  faces,
  stage,
}: {
  scene: Scene;
  /** Keys of the rows that shout (`fleetLoudness`). */
  shouts: ReadonlySet<string>;
  now: number;
  faces: Faces;
  stage?: StageExtras;
}) => {
  const { ref, onScroll } = useNewestInView(!!stage, scene, stage?.streaming ?? stage?.message);
  const talks = scene.running.length + scene.finished.length;
  const compact = talks > 1;
  const showMain = scene.toYou || (scene.mainBusy && (talks === 0 || !!stage?.streaming));
  const running = [...scene.running].sort((a, b) => a.at - b.at);
  const toYou = showMain && <ToYou key="to-you" scene={scene} quiet={talks > 0 && !stage} now={now} stage={stage} />;
  const fromYou = scene.userMessage && (
    <div key="from-you" className={styles.fromYou} data-row>
      <div className={styles.bubble}>
        <Tail right />
        <span className={styles.bubbleText} title={scene.userMessage.text}>
          {scene.userMessage.text}
        </span>
      </div>
      <span className={styles.meta}>you · {shortTime(scene.userMessage.at, now)}</span>
    </div>
  );
  // Main's word and yours, in the order they were said.
  const words = (scene.toYou?.at ?? Infinity) < (scene.userMessage?.at ?? -Infinity) ? [toYou, fromYou] : [fromYou, toYou];
  return (
    <div ref={ref} className={styles.scene} data-mood={scene.mood} data-size={stage ? 'stage' : 'small'} onScroll={onScroll}>
      <p className={styles.captionRow}>
        <Caption caption={scene.caption} mood={scene.mood} now={now} />
      </p>
      {scene.earlier.length > 0 && (
        <ul className={styles.earlier} aria-label="Earlier">
          {[...scene.earlier].reverse().map((item) => (
            <EarlierItem key={item.key} item={item} now={now} faces={faces} stage={stage} />
          ))}
        </ul>
      )}
      {[...scene.finished].reverse().map((talk) => (
        <ResultTalk key={talk.key} talk={talk} compact={compact} shout={shouts.has(talk.key)} now={now} faces={faces} stage={stage} />
      ))}
      {words}
      {!showMain && talks === 0 && !scene.userMessage && <p className={styles.nothing}>No messages yet.</p>}
      {running.map((talk, index) => (
        <RunningTalk key={talk.key} talk={talk} index={index} compact={compact} rush={shouts.has(talk.key)} now={now} faces={faces} stage={stage} />
      ))}
    </div>
  );
};

export default FleetScene;
