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
import { getMessage } from '../../assistant/client';
import MarkdownMessage from '../Chat/MarkdownMessage';
import ImageAttachment, { useImageSrc } from '../AssistantChat/ImageAttachment';
import { useAppTheme } from '../../hooks/useAppTheme';
import { faceOf, identityRingColor, type AgentIdentity } from '../Agents/agentIdentity';
import { fullMessageText, type FleetImage } from '../../fleet/fleetModel';
import {
  isSpeech,
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
import { Caption, MAIN_FACE, OutcomeIcon, ThoughtCloud, Who, type Cast } from './FleetFace';
import FleetZoom, { Tail, zoomStart, type ZoomStart } from './FleetZoom';
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
const statusOf = (line: FleetLine): 'failed' | 'stopped' | 'working' | 'next' | null =>
  line.outcome === 'failed' || line.outcome === 'stopped'
    ? line.outcome
    : line.live === 'running'
      ? 'working'
      : line.live === 'queued'
        ? 'next'
        : null;

type Open = (line: FleetLine, origin: HTMLElement) => void;

/** A bubble pressed by a click, Enter or Space; the card under it is not selected, and a link or button in it is its own. */
/** What a bubble leaves a click or key to: its links and buttons. */
const OWNED = ':is(a, button)';

const pressable = (onPress: (target: HTMLElement) => void) => {
  const ownedInside = (e: SyntheticEvent<HTMLElement>) => e.target instanceof Element && e.target.closest(OWNED) !== null;
  return {
    role: 'button',
    tabIndex: 0,
    onClick: (e: MouseEvent<HTMLElement>) => {
      e.stopPropagation();
      if (!ownedInside(e)) onPress(e.currentTarget);
    },
    onKeyDown: (e: KeyboardEvent<HTMLElement>) => {
      if (e.key !== 'Enter' && e.key !== ' ') return;
      e.stopPropagation();
      if (ownedInside(e)) return;
      e.preventDefault();
      onPress(e.currentTarget);
    },
  };
};

/** A date and time, for the zoomed message. */
const fullTime = (at: number): string =>
  new Date(at).toLocaleString([], { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });

/**
 * A bubble's first words, small: when, how a task ended or that it waits its
 * turn, and whom it is for when no face there says so. The faces' names are
 * the only names a bubble shows.
 */
const Stamp = ({ line, time, route = false }: { line: FleetLine; time: string; route?: boolean }) => {
  const status = statusOf(line);
  return (
    <span className={styles.stamp} data-outcome={line.outcome ?? undefined} data-live={line.live ?? undefined}>
      <OutcomeIcon line={line} />
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

const Outcome = ({ line, time }: { line: FleetLine; time: string }) => {
  const status = statusOf(line);
  return (
    <span className={styles.lineStatus} data-outcome={line.outcome ?? undefined} data-live={line.live ?? undefined}>
      <OutcomeIcon line={line} />
      {status && `${status} · `}
      {time}
    </span>
  );
};

/** Pictures a bubble shows; past them, how many more. */
const THUMBS = 3;

/**
 * A bubble's pictures, as the chat draws them: each a thumbnail that opens
 * the image (a button, so the bubble leaves the click to it). The boxes have
 * a fixed size, so the bubble is as tall before they load as after (`useFit`).
 */
const Thumbs = ({ workspaceId, images }: { workspaceId: string; images: readonly FleetImage[] }) =>
  images.length > 0 && (
    <div className={styles.thumbs}>
      {images.slice(0, THUMBS).map((part) => (
        <span key={part.id} className={styles.thumb}>
          <ImageAttachment workspaceId={workspaceId} part={part} />
        </span>
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
  expanded,
  onToggle,
}: {
  workspaceId: string;
  line: FleetLine;
  shout: boolean;
  now: number;
  faces: Cast;
  open: Open;
  /** Main's bubble, open in place. */
  expanded: boolean;
  onToggle: (key: string) => void;
}) => {
  const from = faceFor(line.from, faces);
  const to = faceFor(line.to, faces);
  const thinking = line.live === 'running';
  const style = useAuthorColor(from);
  const time = shortTime(line.at, now);
  if (!from) {
    // No faces: a word to Main goes without saying, one to a task names it.
    return (
      <div className={styles.fromYou} data-row aria-haspopup="dialog" {...pressable((target) => open(line, target))}>
        <div className={styles.bubble} data-bubble>
          <Tail right />
          <span className={styles.bubbleText} title={line.text}>
            <Stamp line={line} time={time} route={line.to.kind === 'agent'} />
            <span>{line.text}</span>
          </span>
          <Thumbs workspaceId={workspaceId} images={line.images} />
        </div>
      </div>
    );
  }
  const fromExpression: Expression | undefined = line.outcome === 'done' ? 'done' : undefined;
  const speech = isSpeech(line);
  return (
    <div
      className={`${styles.said} ${thinking ? fx.tilt : ''}`}
      data-thinking={thinking || undefined}
      data-row
      style={style}
      {...(speech
        ? { 'aria-expanded': expanded, ...pressable(() => onToggle(line.key)) }
        : { 'aria-haspopup': 'dialog' as const, ...pressable((target) => open(line, target)) })}
    >
      {shout && thinking && <SpeedLines />}
      {shout && <Sfx word={thinking ? 'GO!' : 'DONE!'} tone={thinking ? 'go' : 'done'} level={shoutLevel(line.at, now)} />}
      <Who identity={from} name={nameOf(line.from)} size={36} expression={fromExpression} />
      <div className={styles.say}>
        <div className={styles.bubble} data-bubble>
          <Tail />
          {speech ? (
            // Still being written, it shows its newest lines; done, its first.
            <div className={`${styles.bubbleText} ${styles.speech}`} data-newest={(line.writing && !expanded) || undefined} data-open={expanded || undefined}>
              <div>
                <Stamp line={line} time={time} />
                <div className={styles.speechText}>
                  {expanded ? <FullSpeech workspaceId={workspaceId} line={line} /> : <FleetMarkdown content={line.markdown} />}
                </div>
              </div>
            </div>
          ) : (
            <>
              <span className={styles.bubbleText} title={line.text}>
                <Stamp line={line} time={time} />
                <span>{line.text}</span>
              </span>
              <Thumbs workspaceId={workspaceId} images={line.images} />
            </>
          )}
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
 * How many highlights fit as bubbles (`splitFeed`). A new set of lines, a
 * bubble opened or closed, or a new card size, is first laid out all as
 * bubbles and measured; the split is set in the same layout pass, so the
 * measuring layout is never painted.
 */
const useFit = (lines: readonly FleetLine[], expanded: string | null) => {
  const ref = useRef<HTMLDivElement | null>(null);
  const signature = `${expanded}\n${lines.map((l) => `${l.key}|${l.live}|${l.outcome}|${l.writing}|${l.images.length}|${l.text}`).join('\n')}`;
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

  return { ref, bubbles: expanded || measuring ? lines.length : fit.bubbles, rows: expanded || measuring ? 0 : fit.rows };
};

/** A truncated message's full text, once fetched; the preview until then, or if it can't be. */
const useFullText = (workspaceId: string, line: FleetLine): string => {
  // Only a cut message is fetched, and it can't grow: a live update replaces it whole.
  const cut = line.message?.truncated ? line.message : null;
  const sessionId = cut?.sessionId;
  const messageId = cut?.id;
  const [full, setFull] = useState<{ id: string; text: string } | null>(null);
  useEffect(() => {
    if (!sessionId || !messageId) return undefined;
    let current = true;
    getMessage(workspaceId, sessionId, messageId)
      .then((m) => current && setFull({ id: messageId, text: fullMessageText(m) }))
      .catch(() => undefined);
    return () => {
      current = false;
    };
  }, [workspaceId, sessionId, messageId]);
  return full && full.id === messageId ? full.text : line.markdown;
};

/** Main's whole bubble, fetched when its text was cut. */
const FullSpeech = ({ workspaceId, line }: { workspaceId: string; line: FleetLine }) => (
  <FleetMarkdown content={useFullText(workspaceId, line)} />
);

/** A picture in the zoom, larger, in a box as tall loaded as not, so the zoom's measured height holds. */
const ZoomImage = ({ workspaceId, part }: { workspaceId: string; part: FleetImage }) => {
  const src = useImageSrc(workspaceId, part.path);
  return (
    <span className={styles.zoomImage}>
      {src ? <img src={src} alt={part.filename ?? 'attached image'} /> : src === false && 'Image unavailable'}
    </span>
  );
};

/** The zoomed highlight: who said it to whom and when, then the whole message as markdown and its pictures; Main's charts as its bubble draws them. */
const Zoomed = ({ workspaceId, line }: { workspaceId: string; line: FleetLine }) => {
  const text = useFullText(workspaceId, line);
  return (
    <div className={styles.zoomText}>
      <p className={styles.zoomStamp}>
        <Route line={line} /> · <Outcome line={line} time={fullTime(line.at)} />
      </p>
      <MarkdownMessage content={text} components={isSpeech(line) ? MINIATURE : undefined} />
      {line.images.map((part) => (
        <ZoomImage key={part.id} workspaceId={workspaceId} part={part} />
      ))}
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
 * A row, or a bubble of anyone but Main, zooms out to the whole message;
 * Main's bubble opens in place, one at a time. A chart in Main's words opens
 * the workspace (`onOpen`).
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
  const [expanded, setExpanded] = useState<string | null>(null);
  const toggle = useCallback((key: string) => setExpanded((k) => (k === key ? null : key)), []);
  const onChart = useCallback(() => onOpen(workspaceId), [onOpen, workspaceId]);
  const openKey = feed.lines.some((line) => line.key === expanded) ? expanded : null;
  const { ref, bubbles: count, rows: rowCount } = useFit(feed.lines, openKey);
  useEffect(() => {
    const card = ref.current?.parentElement;
    if (!card || !openKey) return undefined;
    const closeOutside = (event: globalThis.MouseEvent) => {
      if (event.target instanceof Element && event.target.closest(`[aria-expanded="true"], [data-bubble] ${OWNED}`)) return;
      setExpanded(null);
    };
    card.addEventListener('click', closeOutside, true);
    return () => card.removeEventListener('click', closeOutside, true);
  }, [openKey, ref]);
  const [zoom, setZoom] = useState<Zoom | null>(null);
  const open = useCallback<Open>(
    (line, origin) =>
      setZoom((z) => {
        if (z) return z;
        const start = zoomStart(origin);
        return faceFor(line.from, faces) ? { line, origin, start } : { line, origin, start: { ...start, look: { ...YOU_LOOK, ...start.look } } };
      }),
    [faces]
  );
  const closed = useCallback(() => setZoom(null), []);
  const split = feed.lines.length - count;
  const rows = feed.lines.slice(Math.max(0, split - rowCount), split);
  const bubbles = feed.lines.slice(split);
  return (
    <ChartOpen.Provider value={onChart}>
      <div ref={ref} className={styles.scene} data-mood={feed.mood} data-expanded={!!openKey || undefined}>
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
          <Said
            key={line.key}
            workspaceId={workspaceId}
            line={line}
            shout={shouts.has(line.key)}
            now={now}
            faces={faces}
            open={open}
            expanded={openKey === line.key}
            onToggle={toggle}
          />
        ))}
        {feed.lines.length === 0 && <p className={styles.nothing}>No messages yet.</p>}
        {zoom && (
          <FleetZoom
            origin={zoom.origin}
            start={zoom.start}
            faceIdentity={faceFor(zoom.line.from, faces)}
            label={`Message from ${nameOf(zoom.line.from)}`}
            onClosed={closed}
          >
            <Zoomed workspaceId={workspaceId} line={zoom.line} />
          </FleetZoom>
        )}
      </div>
    </ChartOpen.Provider>
  );
};

export default FleetFeed;
