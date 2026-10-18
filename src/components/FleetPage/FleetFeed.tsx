import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
} from 'react';
import { flushSync } from 'react-dom';
import { getMessage } from '../../assistant/client';
import MarkdownMessage from '../Chat/MarkdownMessage';
import { useAppTheme } from '../../hooks/useAppTheme';
import {
  faceOf,
  identityRingColor,
  mainIdentity,
  type AgentActivity,
  type AgentIdentity,
} from '../Agents/agentIdentity';
import {
  captionText,
  fullMessageText,
  splitFeed,
  shortTime,
  shoutLevel,
  type FleetCaption,
  type FleetFeed as Feed,
  type FleetLine,
  type FleetSpeaker,
} from '../../fleet/fleetModel';
import fx, { FaceExpression, Sfx, SpeedLines, type Expression } from './FleetEffects';
import FleetFace, { MAIN_FACE, type Cast, type CastIdentity } from './FleetFace';
import FleetZoom, { Tail, zoomStart, type ZoomStart } from './FleetZoom';
import styles from './FleetPage.module.css';

type Faces = Cast;

/** The caption box: when, worded against the page's clock. */
export const Caption = ({ caption, mood, now }: { caption: FleetCaption; mood: string; now: number }) => (
  <span className={styles.caption} data-mood={mood}>
    {captionText(caption, now)}
  </span>
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
  identity: CastIdentity;
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
    <span className={`${fx.actor} ${styles.face}`} style={{ width: size, height: size }} data-anchor>
      {ghosts &&
        [0, 1].map((i) => (
          <span key={i} className={fx.ghost} aria-hidden="true">
            <FleetFace identity={identity} size={size} activity={activity} ring={false} />
          </span>
        ))}
      <FleetFace identity={identity} size={size} activity={activity} ring={false} />
      {expression && <FaceExpression kind={expression} size={size} />}
    </span>
    {children}
    <figcaption>{name}</figcaption>
  </figure>
);

/** The effects drawn over an agent's line (speed lines, GO!, the tilted frame) take its colour; its bubble does not. */
const YOU_LOOK = { '--bubble-border': 'var(--fleet-you-bubble)' } as CSSProperties;

const useAuthorColor = (author: AgentIdentity | null) => {
  const theme = useAppTheme();
  return author ? ({ '--agent-color': identityRingColor(author, theme) } as CSSProperties) : undefined;
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
const faceFor = (s: FleetSpeaker, faces: Faces): CastIdentity | null =>
  s.kind === 'agent' ? faceOf(faces, s.agent.seed) : s.kind === 'main' ? (faces[MAIN_FACE] ?? mainIdentity()) : null;

/**
 * `Main → Researcher`, with how a task ended or that it waits its turn. Main's
 * word to you names only Main: the face and the tail already say it's yours.
 */
const Route = ({ line }: { line: FleetLine }) => (
  <>
    <b>{nameOf(line.from)}</b>
    {line.to.kind !== 'you' && (
      <>
        {' → '}
        <b>{nameOf(line.to)}</b>
      </>
    )}
    {line.live === 'queued' && ' · next'}
  </>
);

type Open = (line: FleetLine, origin: HTMLElement) => void;

/** A bubble zooms on a click, Enter or Space; the card under it is not selected. */
const zoomable = (line: FleetLine, open: Open) => ({
  role: 'button',
  tabIndex: 0,
  'aria-haspopup': 'dialog' as const,
  onClick: (e: MouseEvent<HTMLElement>) => {
    e.stopPropagation();
    open(line, e.currentTarget);
  },
  onKeyDown: (e: KeyboardEvent<HTMLElement>) => {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    e.preventDefault();
    e.stopPropagation();
    open(line, e.currentTarget);
  },
});

/** A date and time, for the zoomed message. */
const fullTime = (at: number): string =>
  new Date(at).toLocaleString([], { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });

/**
 * A bubble's first words, small: when, how a task ended or that it waits its
 * turn, and whom it is for when no face there says so. The faces' names are
 * the only names a bubble shows.
 */
const Stamp = ({ line, time, route = false }: { line: FleetLine; time: string; route?: boolean }) => {
  const status =
    line.outcome === 'failed' || line.outcome === 'stopped'
      ? line.outcome
      : line.live === 'running'
        ? 'working'
        : line.live === 'queued'
          ? 'next'
          : null;
  return (
    <span className={styles.stamp} data-outcome={line.outcome ?? undefined} data-live={line.live ?? undefined}>
      {line.outcome === 'failed' ? <Cross /> : line.outcome === 'done' ? <Check /> : null}
      {time}
      {route && line.to.kind !== 'you' && (
        <>
          {' → '}
          <b>{nameOf(line.to)}</b>
        </>
      )}
      {status && ` · ${status}`}
      {' · '}
    </span>
  );
};

const Outcome = ({ line, time }: { line: FleetLine; time: string }) => (
  <span className={styles.lineStatus} data-outcome={line.outcome ?? undefined} data-live={line.live ?? undefined}>
    {line.outcome === 'failed' ? <Cross /> : line.outcome === 'done' ? <Check /> : null}
    {line.outcome === 'failed' ? 'failed · ' : line.outcome === 'stopped' ? 'stopped · ' : line.live === 'running' ? 'working · ' : ''}
    {time}
  </span>
);

/** An older highlight, collapsed to one line. A task in flight is in the crew strip, thinking. */
const Row = ({ line, now, faces, open }: { line: FleetLine; now: number; faces: Faces; open: Open }) => {
  const from = faceFor(line.from, faces);
  const to = faceFor(line.to, faces);
  return (
    <li className={styles.row} data-row>
      <button
        type="button"
        className={styles.rowButton}
        aria-haspopup="dialog"
        onClick={(e) => {
          e.stopPropagation();
          open(line, e.currentTarget);
        }}
      >
        <span className={styles.rowFaces} aria-hidden="true">
          {from && (
            <span className={styles.rowFace} data-anchor>
              <FleetFace identity={from} size={16} ring={false} />
            </span>
          )}
          {to && (
            <span className={styles.rowFace} data-anchor>
              <FleetFace identity={to} size={16} ring={false} />
            </span>
          )}
        </span>
        <span className={styles.rowText} title={line.text}>
          <Route line={line} /> {line.headline}
        </span>
        <Outcome line={line} time={shortTime(line.at, now)} />
      </button>
    </li>
  );
};

/**
 * One of the newest highlights, as a bubble: the speaker's face, the words,
 * and whom they are for. A task handed over shows its agent thinking; the
 * page's newest one rushes in with a "GO!", and the loud card's newest
 * result cheers "DONE!".
 */
const Said = ({
  line,
  shout,
  now,
  faces,
  open,
}: {
  line: FleetLine;
  shout: boolean;
  now: number;
  faces: Faces;
  open: Open;
}) => {
  const from = faceFor(line.from, faces);
  const to = faceFor(line.to, faces);
  const thinking = line.live === 'running';
  const style = useAuthorColor(from);
  const time = shortTime(line.at, now);
  if (!from) {
    // No faces: a word to Main goes without saying, one to a task names it.
    return (
      <div className={styles.fromYou} data-row {...zoomable(line, open)}>
        <div className={styles.bubble} data-bubble>
          <Tail right />
          <span className={styles.bubbleText} title={line.text}>
            <Stamp line={line} time={time} route={line.to.kind === 'agent'} />
            <span>{line.text}</span>
          </span>
        </div>
      </div>
    );
  }
  const fromExpression: Expression | undefined = line.outcome === 'done' ? 'done' : undefined;
  return (
    <div
      className={`${styles.said} ${thinking ? fx.tilt : ''}`}
      data-thinking={thinking || undefined}
      data-row
      style={style}
      {...zoomable(line, open)}
    >
      {shout && thinking && <SpeedLines />}
      {shout && <Sfx word={thinking ? 'GO!' : 'DONE!'} tone={thinking ? 'go' : 'done'} level={shoutLevel(line.at, now)} />}
      <Who identity={from} name={nameOf(line.from)} size={36} expression={fromExpression} />
      <div className={styles.say}>
        <div className={styles.bubble} data-bubble>
          <Tail />
          <span className={styles.bubbleText} title={line.text}>
            <Stamp line={line} time={time} />
            <span>{line.text}</span>
          </span>
        </div>
      </div>
      {to && (
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
      )}
    </div>
  );
};

/** A row's height with the scene's gap, and the rule under the last row (`.row` in the CSS). */
const ROW_PX = 22 + 8;
const ROWS_EDGE_PX = 3;
const GAP_PX = 8;

interface Fit {
  signature: string;
  width: number;
  height: number;
  bubbles: number;
  rows: number;
}

/**
 * How many highlights fit as bubbles (`splitFeed`). A new set of lines, or a
 * new card size, is first laid out all as bubbles and measured; the split is
 * set in the same layout pass, so the measuring layout is never painted.
 */
const useFit = (lines: readonly FleetLine[]) => {
  const ref = useRef<HTMLDivElement | null>(null);
  const signature = lines.map((l) => `${l.key}|${l.live}|${l.outcome}|${l.text}`).join('\n');
  const [fit, setFit] = useState<Fit | null>(null);
  const fitRef = useRef(fit);
  const measuring = fit?.signature !== signature;

  useLayoutEffect(() => {
    fitRef.current = fit;
    const node = ref.current;
    if (!node) return;
    if (measuring) {
      const caption = node.firstElementChild as HTMLElement | null;
      const heights = [...node.querySelectorAll<HTMLElement>(':scope > [data-row]')].map((b) => b.offsetHeight + GAP_PX);
      const available = node.clientHeight - (caption?.offsetHeight ?? 0);
      setFit({ signature, width: node.clientWidth, height: node.clientHeight, ...splitFeed(heights, available, ROW_PX, ROWS_EDGE_PX) });
    } else if (fit.rows > 0 && node.scrollHeight > node.clientHeight + 1) {
      // A row taller than reckoned: drop the oldest rather than clip the newest bubble.
      setFit({ ...fit, rows: fit.rows - 1 });
    }
  }, [measuring, fit, signature]);

  useLayoutEffect(() => {
    const node = ref.current;
    if (!node || typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver(() => {
      const current = fitRef.current;
      if (!current || (current.width === node.clientWidth && current.height === node.clientHeight)) return;
      flushSync(() => setFit(null));
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  return { ref, bubbles: measuring ? lines.length : fit.bubbles, rows: measuring ? 0 : fit.rows };
};

/** A truncated message's full text, once fetched; the preview until then, or if it can't be. */
const useFullText = (workspaceId: string, line: FleetLine): string => {
  const [full, setFull] = useState<string | null>(null);
  useEffect(() => {
    const message = line.message;
    if (!message?.truncated) return undefined;
    let current = true;
    getMessage(workspaceId, message.sessionId, message.id)
      .then((m) => current && setFull(fullMessageText(m)))
      .catch(() => undefined);
    return () => {
      current = false;
    };
  }, [workspaceId, line]);
  return full || line.markdown;
};

/** The zoomed highlight: who said it to whom and when, then the whole message as markdown. */
const Zoomed = ({ workspaceId, line }: { workspaceId: string; line: FleetLine }) => {
  const text = useFullText(workspaceId, line);
  return (
    <div className={styles.zoomText}>
      <p className={styles.zoomStamp}>
        <Route line={line} /> · <Outcome line={line} time={fullTime(line.at)} />
      </p>
      <MarkdownMessage content={text} />
    </div>
  );
};

interface Zoom {
  line: FleetLine;
  origin: HTMLElement;
  start: ZoomStart;
}

/**
 * A miniature: the workspace's conversation as highlights, in the order it
 * happened. The newest are bubbles, as many as fit; up to `ROWS` older ones
 * collapse to one-line rows above them, the newest row nearest the bubbles.
 * A bubble or a row zooms out to the whole message.
 */
const FleetFeed = ({
  workspaceId,
  feed,
  shouts,
  now,
  faces,
}: {
  workspaceId: string;
  feed: Feed;
  /** Keys of the lines that shout (`fleetLoudness`). */
  shouts: ReadonlySet<string>;
  now: number;
  faces: Faces;
}) => {
  const { ref, bubbles: count, rows: rowCount } = useFit(feed.lines);
  const [zoom, setZoom] = useState<Zoom | null>(null);
  const open = useCallback<Open>(
    (line, origin) =>
      setZoom((z) => {
        if (z) return z;
        const start = zoomStart(origin);
        // A row has no bubble to copy: the user's words still take the user's bubble.
        return faceFor(line.from, faces) ? { line, origin, start } : { line, origin, start: { ...start, look: { ...YOU_LOOK, ...start.look } } };
      }),
    [faces]
  );
  const closed = useCallback(() => setZoom(null), []);
  const split = feed.lines.length - count;
  const rows = feed.lines.slice(Math.max(0, split - rowCount), split);
  const bubbles = feed.lines.slice(split);
  const zoomFace = zoom && faceFor(zoom.line.from, faces);
  return (
    <div ref={ref} className={styles.scene} data-mood={feed.mood}>
      <p className={styles.captionRow}>
        <Caption caption={feed.caption} mood={feed.mood} now={now} />
      </p>
      {rows.length > 0 && (
        <ul className={styles.rows} aria-label="Earlier">
          {rows.map((line) => (
            <Row key={line.key} line={line} now={now} faces={faces} open={open} />
          ))}
        </ul>
      )}
      {bubbles.map((line) => (
        <Said key={line.key} line={line} shout={shouts.has(line.key)} now={now} faces={faces} open={open} />
      ))}
      {feed.lines.length === 0 && <p className={styles.nothing}>No messages yet.</p>}
      {zoom && (
        <FleetZoom
          origin={zoom.origin}
          start={zoom.start}
          face={zoomFace && zoom.start.face && <FleetFace identity={zoomFace} size={Math.round(zoom.start.face.width)} ring={false} />}
          label={`Message from ${nameOf(zoom.line.from)}`}
          onClosed={closed}
        >
          <Zoomed workspaceId={workspaceId} line={zoom.line} />
        </FleetZoom>
      )}
    </div>
  );
};

export default FleetFeed;
