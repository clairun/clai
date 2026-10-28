import {
  useState,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';
import AgentAvatar from '../Agents/AgentAvatar';
import type { AgentIdentity } from '../Agents/agentIdentity';
import styles from './FleetPage.module.css';

/** The zoom's transition, `--zoom-ms` in the CSS; the two move together. */
const CLOSE_MS = 220;

export const moves = () =>
  typeof window.matchMedia === 'function' && !window.matchMedia('(prefers-reduced-motion: reduce)').matches;

/** Viewport margin; widest zoom; face to bubble, as on the card; a made-up bubble for a row or a face. */
const MARGIN = 16;
const WIDEST = 560;
const GAP = 16;
const SMALL = { width: 64, height: 36 };
/** The tail's box (`Tail`). */
const TAIL_HEIGHT = 22;

/** A hand-drawn speech-bubble tail; `right` points at the face on the right. */
export const Tail = ({ right = false }: { right?: boolean }) => (
  <svg className={`${styles.tail} ${right ? styles.tailRight : ''}`} width="24" height="22" viewBox="0 0 28 24" aria-hidden="true" data-tail>
    <path className={styles.tailFill} d="M28 3 C20 6 10 12 1 21 C11 17 19 15.5 28 15.5 Z" />
    <path className={styles.tailStroke} d="M25 3.6 C18 6.6 10 12 1 21 C11 17 18 15.6 25 15.5" />
  </svg>
);

interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Where a zoom starts: the card bubble as it is, and where its tail meets the face. */
export interface ZoomStart {
  rect: Box;
  /** The tail's top, in the viewport; it never moves. */
  tailY: number;
  /** The bubble's side the tail is on, towards the face. */
  side: 'left' | 'right';
  /** The card bubble, hidden while its zoom is open; `null` for a row or a face. */
  source: HTMLElement | null;
  /** The bubble's own look, copied, so the zoom starts as its pixel copy. */
  look: CSSProperties;
  /** The speaker's face, drawn over the scrim where it is on the card. */
  face: DOMRect | null;
}

const box = (r: DOMRect): Box => ({ left: r.left, top: r.top, width: r.width, height: r.height });

/**
 * A zoom's start, read off what was clicked: its bubble (`data-bubble`) and
 * tail, or, for a row or a face with no bubble, a small one just right of
 * the face.
 */
export const zoomStart = (origin: HTMLElement): ZoomStart => {
  const anchor = origin.querySelector<HTMLElement>('[data-anchor]');
  const face = anchor?.getBoundingClientRect() ?? null;
  const bubble = origin.querySelector<HTMLElement>('[data-bubble]');
  if (bubble) {
    const rect = box(bubble.getBoundingClientRect());
    const tail = bubble.querySelector('[data-tail]')?.getBoundingClientRect();
    const css = getComputedStyle(bubble);
    return {
      rect,
      tailY: tail ? tail.top : rect.top + rect.height / 2 - TAIL_HEIGHT / 2,
      side: tail && tail.left > rect.left + rect.width / 2 ? 'right' : 'left',
      source: bubble,
      face,
      look: {
        '--bubble-border': css.borderTopColor || undefined,
        padding: css.padding || undefined,
        fontSize: css.fontSize || undefined,
        lineHeight: css.lineHeight || undefined,
        borderWidth: css.borderTopWidth || undefined,
        borderRadius: css.borderTopLeftRadius || undefined,
        background: css.backgroundColor || undefined,
      } as CSSProperties,
    };
  }
  const at = face ?? origin.getBoundingClientRect();
  const mid = at.top + at.height / 2;
  return {
    rect: { left: at.right + GAP, top: mid - SMALL.height / 2, ...SMALL },
    tailY: mid - TAIL_HEIGHT / 2,
    side: 'left',
    source: null,
    face,
    look: {},
  };
};

/** A zoom narrower than this on the bubble's own side opens on the face's other side. */
const NARROWEST = 360;

/**
 * Where the zoom ends, given its content's height at the width it gets. It
 * widens away from the face, on the bubble's side when there is room, else
 * on the face's other side (a card in the right column grows left); its
 * tail stays at the same height on the face. It grows down from a bubble in
 * the top half and up from one in the bottom half, never past the window:
 * a long message scrolls inside.
 */
export const zoomGeometry = (
  start: ZoomStart,
  natural: number,
  vw: number,
  vh: number
): Box & { tailAt: number; side: 'left' | 'right' } => {
  const { rect, tailY, face } = start;
  const want = Math.min(WIDEST, vw * 0.6);
  // The edge the tail is on, and across the face, the edge it would be on.
  const near = start.side === 'left' ? rect.left : rect.left + rect.width;
  const far = face ? (start.side === 'left' ? face.left - GAP : face.right + GAP) : near;
  const roomOf = (edge: number, side: 'left' | 'right') => (side === 'left' ? vw - MARGIN - edge : edge - MARGIN);
  const flipped: 'left' | 'right' = start.side === 'left' ? 'right' : 'left';
  const stay = !face || roomOf(near, start.side) >= Math.min(want, NARROWEST) || roomOf(near, start.side) >= roomOf(far, flipped);
  const side = stay ? start.side : flipped;
  const edge = stay ? near : far;
  const width = Math.min(Math.max(want, rect.width), roomOf(edge, side));
  const left = side === 'left' ? edge : edge - width;
  const height = Math.max(rect.height, Math.min(natural, vh - 2 * MARGIN));
  const down = rect.top + rect.height / 2 < vh / 2;
  const top = Math.min(Math.max(down ? rect.top : rect.top + rect.height - height, MARGIN), Math.max(MARGIN, vh - MARGIN - height));
  return { left, top, width, height, tailAt: tailY - top, side };
};

/** `tailAt` is from the bubble's outer edge; the tail is placed inside its border. */
const place = (el: HTMLElement, tail: HTMLElement | null, at: Box, tailAt: number) => {
  el.style.left = `${at.left}px`;
  el.style.top = `${at.top}px`;
  el.style.width = `${at.width}px`;
  el.style.height = `${at.height}px`;
  if (tail) tail.style.top = `${tailAt - (parseFloat(getComputedStyle(el).borderTopWidth) || 0)}px`;
};

const FOCUSABLE = 'a[href], button, input, textarea, select, [tabindex]:not([tabindex="-1"])';

/**
 * A highlight zoomed out of its card, in a page-level layer over the dimmed
 * Fleet. It starts as the card bubble and grows (`zoomGeometry`) with its
 * tail on the speaker's face, which is drawn over the scrim where it is on
 * the card (`faceIdentity`). The card bubble hides from opening until the zoom has
 * shrunk back, so only one bubble ever shows. Esc, a click outside or on the
 * bubble shrinks it back; focus returns to `origin`.
 */
const FleetZoom = ({
  origin,
  start,
  faceIdentity,
  label,
  onClosed,
  children,
}: {
  origin: HTMLElement;
  start: ZoomStart;
  /** The speaker's face; none for the user. */
  faceIdentity?: AgentIdentity | null;
  label: string;
  onClosed: () => void;
  children: ReactNode;
}) => {
  const panelRef = useRef<HTMLDivElement | null>(null);
  // Which side the tail ends on does not depend on the content's height.
  const [side] = useState(() => zoomGeometry(start, 0, window.innerWidth, window.innerHeight).side);
  const scrimRef = useRef<HTMLDivElement | null>(null);
  const closing = useRef(false);
  const tailOf = (panel: HTMLElement) => panel.querySelector<HTMLElement>(':scope > [data-tail]');

  useLayoutEffect(() => {
    const source = start.source;
    // Hidden, not removed: the card keeps its layout, and only one bubble shows.
    source?.style.setProperty('visibility', 'hidden');
    return () => {
      source?.style.removeProperty('visibility');
    };
  }, [start.source]);

  useLayoutEffect(() => {
    const panel = panelRef.current!;
    const tail = tailOf(panel);
    // Measure the content at its final width, before anything is painted.
    const { width } = zoomGeometry(start, 0, window.innerWidth, window.innerHeight);
    panel.style.width = `${width}px`;
    panel.style.height = 'auto';
    const end = zoomGeometry(start, panel.offsetHeight, window.innerWidth, window.innerHeight);
    // Start as the card bubble, with no transition; then grow.
    const scrim = scrimRef.current!;
    panel.dataset.phase = scrim.dataset.phase = 'from';
    place(panel, tail, start.rect, start.tailY - start.rect.top);
    void panel.offsetWidth;
    delete panel.dataset.phase;
    delete scrim.dataset.phase;
    place(panel, tail, end, end.tailAt);
    panel.focus({ preventScroll: true });
  }, [start]);

  const close = useCallback(() => {
    const panel = panelRef.current;
    if (closing.current || !panel) return;
    closing.current = true;
    const done = () => {
      if (origin.isConnected) origin.focus({ preventScroll: true });
      onClosed();
    };
    if (!moves()) return done();
    scrimRef.current!.dataset.phase = 'closing';
    place(panel, tailOf(panel), start.rect, start.tailY - start.rect.top);
    window.setTimeout(done, CLOSE_MS);
  }, [origin, onClosed, start]);

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      e.stopPropagation();
      close();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [close]);

  // Focus stays in the panel while it is open.
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Enter' || e.key === ' ') {
      if (e.target === e.currentTarget) {
        e.preventDefault();
        close();
      }
      return;
    }
    if (e.code !== 'Tab') return;
    const order = [e.currentTarget, ...e.currentTarget.querySelectorAll<HTMLElement>(FOCUSABLE)];
    const at = Math.max(0, order.indexOf(document.activeElement as HTMLElement));
    e.preventDefault();
    order[(at + (e.shiftKey ? -1 : 1) + order.length) % order.length]!.focus();
  };

  return createPortal(
    <div className={styles.zoomLayer}>
      <div ref={scrimRef} className={styles.zoomScrim} onClick={close} aria-hidden="true" />
      {faceIdentity && start.face && (
        <span
          className={styles.zoomFace}
          style={{ left: start.face.left, top: start.face.top, width: start.face.width, height: start.face.height }}
          aria-hidden="true"
        >
          <AgentAvatar identity={faceIdentity} size={Math.round(start.face.width)} ring={false} />
        </span>
      )}
      <div
        ref={panelRef}
        className={`${styles.bubble} ${styles.zoomPanel}`}
        data-tail-side={side}
        style={start.look}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        tabIndex={-1}
        onKeyDown={onKeyDown}
        onClick={(e) => {
          // Links and selected text are the panel's; any other click zooms back.
          if ((e.target as HTMLElement).closest('a') || window.getSelection()?.toString()) return;
          close();
        }}
      >
        <Tail right={side === 'right'} />
        {children}
      </div>
    </div>,
    document.body
  );
};

export default FleetZoom;
