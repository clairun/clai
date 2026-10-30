import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type MouseEvent,
  type SyntheticEvent,
} from 'react';
import { flushSync } from 'react-dom';
import MarkdownMessage from '../Chat/MarkdownMessage';
import { useImageSrc } from '../AssistantChat/ImageAttachment';
import { useAppTheme } from '../../hooks/useAppTheme';
import { faceOf, identityRingColor, type AgentIdentity } from '../Agents/agentIdentity';
import { useFleetStore } from '../../fleet/fleetStore';
import type { ImagePart } from '../../assistant/imageParts';
import {
  splitFeed,
  shortTime,
  type FleetFeed as Feed,
  type FleetLine,
  type FleetSpeaker,
} from '../../fleet/fleetFeed';
import { shoutLevel } from '../../fleet/fleetCrew';
import fx, { Sfx, SpeedLines, type Expression } from './FleetEffects';
import AgentAvatar from '../Agents/AgentAvatar';
import FleetMarkdown, { ChartOpen, MINIATURE } from './FleetMarkdown';
import { Caption, MAIN_FACE, ThoughtCloud, Who, type Cast } from './FleetFace';
import FleetZoom, { Tail, zoomStart, type ZoomStart } from './FleetZoom';
import { useFollowEnd } from './useFollowEnd';
import { useWheelLines } from './useWheelLines';
import styles from './FleetPage.module.css';

/** A row has no bubble to copy: the user's words still take the user's bubble. */
const YOU_LOOK = { '--bubble-border': 'var(--fleet-you-bubble)' } as CSSProperties;

/** The effects drawn over an agent's line (speed lines, GO!, the tilted frame) take its colour; its bubble does not. */
const useAuthorColor = (author: AgentIdentity | null) => {
  const theme = useAppTheme();
  return author ? ({ '--agent-color': identityRingColor(author, theme) } as CSSProperties) : undefined;
};

const nameOf = (s: FleetSpeaker): string => (s.kind === 'you' ? 'You' : s.kind === 'main' ? 'Main' : s.agent.name);
const faceFor = (s: FleetSpeaker, faces: Cast): AgentIdentity | null =>
  s.kind === 'agent' ? faceOf(faces, s.agent.seed) : s.kind === 'main' ? faces[MAIN_FACE] : null;

/** `Main → Researcher`. Main's word to you names only Main: the face and the tail already say it's yours. */
const Route = ({ line }: { line: FleetLine }) => (
  <>
    <b>{nameOf(line.from)}</b>
    {line.to.kind !== 'you' && (
      <>
        {' → '}
        <b>{nameOf(line.to)}</b>
      </>
    )}
  </>
);

/** How a task ended, or that it is working or waits its turn. */
const statusOf = (line: FleetLine): 'done' | 'failed' | 'stopped' | 'working' | 'next' | null =>
  line.outcome
    ? line.outcome
    : line.live === 'running'
      ? 'working'
      : line.live === 'queued'
        ? 'next'
        : null;

type Open = (line: FleetLine, origin: HTMLElement) => void;

/**
 * A bubble pressed by a click, Enter or Space; the card under it is not
 * selected. A link or button in it keeps its click and keys untouched: a
 * link opens through the opener's listener on `window`. A click on its
 * vertical scrollbar, or one that ends a drag selecting its words, is not a
 * press.
 */
const pressable = (onPress: (target: HTMLElement) => void) => {
  const ownedInside = (e: SyntheticEvent<HTMLElement>) => e.target instanceof Element && e.target.closest('a, button') !== null;
  const selecting = (e: MouseEvent<HTMLElement>) => {
    const selection = getSelection();
    return !!selection && !selection.isCollapsed && e.currentTarget.contains(selection.focusNode);
  };
  const onScrollbar = (e: MouseEvent<HTMLElement>) =>
    e.target instanceof HTMLElement && e.target.scrollHeight > e.target.clientHeight && e.nativeEvent.offsetX >= e.target.clientWidth;
  return {
    role: 'button',
    tabIndex: 0,
    onClick: (e: MouseEvent<HTMLElement>) => {
      if (ownedInside(e)) return;
      e.stopPropagation();
      if (!onScrollbar(e) && !selecting(e)) onPress(e.currentTarget);
    },
    onKeyDown: (e: KeyboardEvent<HTMLElement>) => {
      if ((e.key !== 'Enter' && e.key !== ' ') || ownedInside(e)) return;
      e.stopPropagation();
      e.preventDefault();
      onPress(e.currentTarget);
    },
  };
};

/** Time and status stay with the message they describe as the bubble scrolls. */
const MessageTime = ({ line, at, now, route = false }: { line: FleetLine; at: number; now: number; route?: boolean }) => {
  const status = statusOf(line);
  return (
    <span className={styles.messageTime} data-outcome={line.outcome ?? undefined} data-live={line.live ?? undefined}>
      {shortTime(at, now)}
      {route && line.to.kind !== 'you' && (
        <>
          {' → '}
          <b>{nameOf(line.to)}</b>
        </>
      )}
      {status && ` · ${status}`}
    </span>
  );
};

const Outcome = ({ line, time }: { line: FleetLine; time: string }) => {
  const status = statusOf(line);
  return (
    <span className={styles.lineStatus} data-outcome={line.outcome ?? undefined} data-live={line.live ?? undefined}>
      {time}
      {status && ` · ${status}`}
    </span>
  );
};

/** Pictures a bubble shows; past them, how many more. */
const THUMBS = 3;

/** A picture in a box of a fixed size, so what holds it is as tall before it loads as after (`useFit`, the zoom's measure). */
const Picture = ({ workspaceId, part, className }: { workspaceId: string; part: ImagePart; className?: string }) => {
  const src = useImageSrc(workspaceId, part.path);
  return (
    <span className={className}>
      {src ? <img src={src} alt={part.filename ?? 'attached image'} /> : src === false && 'Image unavailable'}
    </span>
  );
};

/** A bubble's pictures, small; a click on one is the bubble's, and its zoom shows them larger. */
const Thumbs = ({ workspaceId, images }: { workspaceId: string; images: readonly ImagePart[] }) =>
  images.length > 0 && (
    <div className={styles.thumbs}>
      {images.slice(0, THUMBS).map((part) => (
        <Picture key={part.id} workspaceId={workspaceId} part={part} className={styles.thumb} />
      ))}
      {images.length > THUMBS && <span className={styles.thumbMore}>+{images.length - THUMBS}</span>}
    </div>
  );

const ImageMark = ({ count }: { count: number }) => (
  <span className={styles.imageMark} role="img" aria-label={count === 1 ? '1 image' : `${count} images`}>
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" aria-hidden="true">
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <circle cx="9" cy="10" r="2" />
      <path d="M21 17l-5-5-9 8" />
    </svg>
    {count > 1 && count}
  </span>
);

/** An older highlight, collapsed to one line. A task in flight is in the crew strip, thinking. */
const Row = ({ line, now, faces, open }: { line: FleetLine; now: number; faces: Cast; open: Open }) => {
  const from = faceFor(line.from, faces);
  const to = faceFor(line.to, faces);
  return (
    <li className={styles.row}>
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
              <AgentAvatar identity={from} size={16} ring={false} />
            </span>
          )}
          {to && (
            <span className={styles.rowFace} data-anchor>
              <AgentAvatar identity={to} size={16} ring={false} />
            </span>
          )}
        </span>
        <span className={styles.rowText} title={line.text}>
          <Route line={line} /> {line.images.length > 0 && <ImageMark count={line.images.length} />}
          {line.headline}
        </span>
        <Outcome line={line} time={shortTime(line.at, now)} />
      </button>
    </li>
  );
};

/** A message the snapshot cut, fetched whole into its card while a bubble or zoom shows it; its preview until then. */
const useWhole = (workspaceId: string, line: FleetLine) => {
  const cut = line.message?.truncated ? line.message : null;
  const sessionId = cut?.sessionId;
  const messageId = cut?.id;
  useEffect(() => {
    if (sessionId && messageId) useFleetStore.getState().loadWhole(workspaceId, sessionId, messageId);
  }, [workspaceId, sessionId, messageId]);
};

/**
 * A bubble's words: the whole message, as markdown, and its pictures. They
 * scroll past the bubble's height; the card's newest follows its end as it
 * grows (`useFollowEnd`), an older one shows its start.
 */
const Words = ({ workspaceId, line, now, route, newest }: { workspaceId: string; line: FleetLine; now: number; route: boolean; newest: boolean }) => {
  useWhole(workspaceId, line);
  const scroller = useFollowEnd(newest, line.markdown);
  useWheelLines(scroller, true);
  return (
    <>
      <div ref={scroller} className={styles.bubbleText}>
        <div>
          {line.blocks.map((block, i) => (
            <div className={styles.speechText} key={i}>
              <FleetMarkdown
                content={line.markdown.slice(block.start, block.end)}
                lead={<MessageTime line={line} at={block.at} now={now} route={route && i === 0} />}
              />
            </div>
          ))}
        </div>
      </div>
      <Thumbs workspaceId={workspaceId} images={line.images} />
    </>
  );
};

/**
 * One of the newest highlights, as a bubble: the speaker's face, the words,
 * and whom they are for. A task handed over shows its agent thinking; the
 * page's newest one rushes in with a "GO!", and the loud card's newest
 * result cheers "DONE!".
 */
const Said = ({
  workspaceId,
  line,
  shout,
  now,
  faces,
  open,
  newest,
}: {
  workspaceId: string;
  line: FleetLine;
  shout: boolean;
  now: number;
  faces: Cast;
  open: Open;
  /** The card's last bubble. */
  newest: boolean;
}) => {
  const from = faceFor(line.from, faces);
  const to = faceFor(line.to, faces);
  const thinking = line.live === 'running';
  const style = useAuthorColor(from);
  if (!from) {
    // No faces: a word to Main goes without saying, one to a task names it.
    return (
      <div className={styles.fromYou} data-row aria-haspopup="dialog" {...pressable((target) => open(line, target))}>
        <div className={styles.bubble} data-bubble>
          <Tail right />
          <Words workspaceId={workspaceId} line={line} now={now} route={line.to.kind === 'agent'} newest={newest} />
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
      aria-haspopup="dialog"
      {...pressable((target) => open(line, target))}
    >
      {shout && thinking && <SpeedLines />}
      {shout && <Sfx word={thinking ? 'GO!' : 'DONE!'} tone={thinking ? 'go' : 'done'} level={shoutLevel(line.at, now)} />}
      <Who identity={from} name={nameOf(line.from)} size={36} expression={fromExpression} />
      <div className={styles.say}>
        <div className={styles.bubble} data-bubble>
          <Tail />
          <Words workspaceId={workspaceId} line={line} now={now} route={false} newest={newest} />
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
  const signature = lines.map((l) => `${l.key}|${l.live}|${l.outcome}|${l.images.length}|${l.text}`).join('\n');
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

/** The zoomed highlight: who said it to whom and when, then the whole message as markdown and its pictures; charts as its bubble draws them. */
const Zoomed = ({ workspaceId, line, now }: { workspaceId: string; line: FleetLine; now: number }) => {
  useWhole(workspaceId, line);
  return (
    <div className={styles.zoomText}>
      <p className={styles.zoomStamp}>
        <Route line={line} />
      </p>
      {line.blocks.map((block, i) => (
        <div className={styles.zoomMessage} key={i}>
          <MessageTime line={line} at={block.at} now={now} />
          <MarkdownMessage content={line.markdown.slice(block.start, block.end)} components={MINIATURE} />
        </div>
      ))}
      {line.images.map((part) => (
        <Picture key={part.id} workspaceId={workspaceId} part={part} className={styles.zoomImage} />
      ))}
    </div>
  );
};

interface Zoom {
  /** As zoomed; the feed's copy, fetched whole or grown since, is shown while the feed has it. */
  line: FleetLine;
  origin: HTMLElement;
  start: ZoomStart;
  /** `youSaidAt` as it opened. */
  since: number;
}

/**
 * A miniature: the workspace's conversation as highlights, in the order it
 * happened. The newest are bubbles, as many as fit; up to `ROWS` older ones
 * collapse to one-line rows above them, the newest row nearest the bubbles.
 * A row or a bubble zooms out to the whole message, until you say something
 * on the card. A chart in a bubble opens the workspace (`onOpen`).
 */
const FleetFeed = ({
  workspaceId,
  feed,
  shouts,
  now,
  faces,
  onOpen,
}: {
  workspaceId: string;
  feed: Feed;
  /** Keys of the lines that shout (`fleetLoudness`). */
  shouts: ReadonlySet<string>;
  now: number;
  faces: Cast;
  onOpen: (workspaceId: string) => void;
}) => {
  const onChart = useCallback(() => onOpen(workspaceId), [onOpen, workspaceId]);
  const { ref, bubbles: count, rows: rowCount } = useFit(feed.lines);
  const [zoom, setZoom] = useState<Zoom | null>(null);
  const since = feed.youSaidAt;
  const shown = (z: Zoom | null): z is Zoom => z !== null && z.since >= since;
  const open = (line: FleetLine, origin: HTMLElement) =>
    setZoom((z) => {
      if (shown(z)) return z;
      const start = zoomStart(origin);
      return { line, origin, since, start: faceFor(line.from, faces) ? start : { ...start, look: { ...YOU_LOOK, ...start.look } } };
    });
  const closed = useCallback(() => setZoom(null), []);
  const zoomed = shown(zoom) ? { ...zoom, line: feed.lines.find((line) => line.key === zoom.line.key) ?? zoom.line } : null;
  const split = feed.lines.length - count;
  const rows = feed.lines.slice(Math.max(0, split - rowCount), split);
  const bubbles = feed.lines.slice(split);
  return (
    <ChartOpen.Provider value={onChart}>
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
        {bubbles.map((line, i) => (
          <Said
            key={line.key}
            workspaceId={workspaceId}
            line={line}
            shout={shouts.has(line.key)}
            now={now}
            faces={faces}
            open={open}
            newest={i === bubbles.length - 1}
          />
        ))}
        {feed.lines.length === 0 && <p className={styles.nothing}>No messages yet.</p>}
        {zoomed && (
          <FleetZoom
            origin={zoomed.origin}
            start={zoomed.start}
            faceIdentity={faceFor(zoomed.line.from, faces)}
            label={`Message from ${nameOf(zoomed.line.from)}`}
            onClosed={closed}
          >
            <Zoomed workspaceId={workspaceId} line={zoomed.line} now={now} />
          </FleetZoom>
        )}
      </div>
    </ChartOpen.Provider>
  );
};

export default FleetFeed;
