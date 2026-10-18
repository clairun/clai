import { useLayoutEffect, useRef, type CSSProperties, type ReactNode } from 'react';
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
  BUBBLES,
  captionText,
  shortTime,
  shoutLevel,
  type FleetCaption,
  type FleetFeed as Feed,
  type FleetLine,
  type FleetSpeaker,
} from '../../fleet/fleetModel';
import fx, { FaceExpression, Sfx, SpeedLines, type Expression } from './FleetEffects';
import styles from './FleetPage.module.css';

type Faces = Record<string, AgentIdentity>;

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


const nameOf = (s: FleetSpeaker): string => (s.kind === 'you' ? 'You' : s.kind === 'main' ? 'Main' : s.agent.name);
const faceFor = (s: FleetSpeaker, faces: Faces): AgentIdentity | null =>
  s.kind === 'agent' ? faceOf(faces, s.agent.seed) : s.kind === 'main' ? mainIdentity() : null;

/** `Main → Researcher`, with how a task ended or that it waits its turn. */
const Route = ({ line }: { line: FleetLine }) => (
  <>
    <b>{nameOf(line.from)}</b> → <b>{nameOf(line.to)}</b>
    {line.live === 'queued' && ' · next'}
  </>
);

const Outcome = ({ line, now }: { line: FleetLine; now: number }) => (
  <span className={styles.lineStatus} data-outcome={line.outcome ?? undefined} data-live={line.live ?? undefined}>
    {line.outcome === 'failed' ? <Cross /> : line.outcome === 'done' ? <Check /> : null}
    {line.outcome === 'failed' ? 'failed · ' : line.outcome === 'stopped' ? 'stopped · ' : line.live === 'running' ? 'working · ' : ''}
    {shortTime(line.at, now)}
  </span>
);

/** An older highlight, collapsed to one line; a task still in flight keeps its thinking face. */
const Row = ({ line, now, faces }: { line: FleetLine; now: number; faces: Faces }) => {
  const from = faceFor(line.from, faces);
  const to = faceFor(line.to, faces);
  const thinking = line.live === 'running';
  return (
    <li className={styles.row} data-row>
      <span className={styles.rowFaces} aria-hidden="true">
        {from && <AgentAvatar identity={from} size={16} ring={false} />}
        {to && (
          <span className={thinking ? styles.rowThinking : undefined}>
            <AgentAvatar identity={to} size={16} ring={false} activity={thinking ? 'running' : 'none'} />
          </span>
        )}
      </span>
      <span className={styles.rowText} title={line.text}>
        <Route line={line} /> {line.headline}
      </span>
      <Outcome line={line} now={now} />
    </li>
  );
};

/**
 * One of the newest highlights, as a bubble: the speaker's face, the words,
 * and whom they are for. A task handed over shows its agent thinking; the
 * page's newest one rushes in with a "GO!", and the loud card's newest
 * result cheers "DONE!".
 */
const Said = ({ line, shout, now, faces }: { line: FleetLine; shout: boolean; now: number; faces: Faces }) => {
  const from = faceFor(line.from, faces);
  const to = faceFor(line.to, faces);
  const thinking = line.live === 'running';
  const style = useAgentColor((to && line.to.kind === 'agent' ? to : from) ?? mainIdentity());
  const tone = line.outcome ?? (line.live ? 'task' : line.to.kind === 'you' ? 'you' : 'say');
  const meta = (
    <span className={styles.meta}>
      <Route line={line} /> · <Outcome line={line} now={now} />
    </span>
  );
  if (!from) {
    return (
      <div className={styles.fromYou} data-row>
        <div className={styles.bubble}>
          <Tail right />
          <span className={styles.bubbleText} title={line.text}>
            {line.text}
          </span>
        </div>
        {meta}
      </div>
    );
  }
  const fromExpression: Expression | undefined = line.outcome === 'done' ? 'done' : undefined;
  return (
    <div className={`${styles.said} ${thinking ? fx.tilt : ''}`} data-tone={tone} data-row style={style}>
      {shout && thinking && <SpeedLines />}
      {shout && <Sfx word={thinking ? 'GO!' : 'DONE!'} tone={thinking ? 'go' : 'done'} level={shoutLevel(line.at, now)} />}
      <Who identity={from} name={nameOf(line.from)} size={36} expression={fromExpression} />
      <div className={styles.say}>
        <div className={styles.bubble}>
          <Tail />
          <span className={styles.bubbleText} title={line.text}>
            {line.text}
          </span>
        </div>
        {meta}
      </div>
      {to ? (
        <Who
          identity={to}
          name={nameOf(line.to)}
          size={26}
          activity={thinking ? 'running' : 'none'}
          label={thinking ? `${nameOf(line.to)} is working` : undefined}
          expression={thinking ? 'working' : undefined}
          ghosts={shout && thinking}
        >
          {thinking && <ThoughtCloud name={nameOf(line.to)} />}
        </Who>
      ) : (
        <span className={styles.toYouMark}>you</span>
      )}
    </div>
  );
};

/**
 * Keeps the newest highlights in view: whole rows hide from the top, oldest
 * first, until the rest fit under the caption.
 */
const useNewestInView = (feed: Feed) => {
  const ref = useRef<HTMLDivElement | null>(null);
  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return undefined;
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
  }, [feed]);
  return ref;
};

/**
 * A miniature: the workspace's conversation as highlights, in the order it
 * happened. The newest `BUBBLES` are bubbles; everything older is a one-line
 * row above them, the newest row nearest the bubbles.
 */
const FleetFeed = ({
  feed,
  shouts,
  now,
  faces,
}: {
  feed: Feed;
  /** Keys of the lines that shout (`fleetLoudness`). */
  shouts: ReadonlySet<string>;
  now: number;
  faces: Faces;
}) => {
  const ref = useNewestInView(feed);
  const split = Math.max(0, feed.lines.length - BUBBLES);
  const rows = feed.lines.slice(0, split);
  const bubbles = feed.lines.slice(split);
  return (
    <div ref={ref} className={styles.scene} data-mood={feed.mood}>
      <p className={styles.captionRow}>
        <Caption caption={feed.caption} mood={feed.mood} now={now} />
      </p>
      {rows.length > 0 && (
        <ul className={styles.rows} aria-label="Earlier">
          {rows.map((line) => (
            <Row key={line.key} line={line} now={now} faces={faces} />
          ))}
        </ul>
      )}
      {bubbles.map((line) => (
        <Said key={line.key} line={line} shout={shouts.has(line.key)} now={now} faces={faces} />
      ))}
      {feed.lines.length === 0 && <p className={styles.nothing}>No messages yet.</p>}
    </div>
  );
};

export default FleetFeed;
