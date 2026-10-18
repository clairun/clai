import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';
import styles from './FleetPage.module.css';

const CLOSE_MS = 250;

const moves = () =>
  typeof window.matchMedia === 'function' && !window.matchMedia('(prefers-reduced-motion: reduce)').matches;

/** Face to bubble, as on the card; viewport margin; bubble edge to the face's middle; the tail's reach and middle. */
const GAP = 16;
const MARGIN = 16;
const LEAD = 28;
const TAIL_REACH = 22;
const TAIL_MID = 11;

/**
 * Where the bubble grows from the face at `at`: on its own side (`side`) when
 * the viewport has room for a readable width, else on the roomier side; down
 * from a face in the top half, up from one in the bottom half. The tail stays
 * on the face; its tip is the transform origin.
 */
const growFrom = (at: DOMRect, side: 'left' | 'right', vw: number, vh: number) => {
  const want = Math.min(560, vw * 0.6);
  const room = { right: vw - at.right - GAP - MARGIN, left: at.left - GAP - MARGIN };
  const other = side === 'right' ? 'left' : 'right';
  const x = room[side] >= want || room[side] >= room[other] ? side : other;
  const mid = at.top + at.height / 2;
  const y = mid < vh / 2 ? 'down' : 'up';
  // The distance from the viewport edge the bubble grows away from, and the tail's from the bubble's.
  const edge = Math.max(MARGIN, (y === 'down' ? mid : vh - mid) - LEAD);
  const tail = Math.max(4, (y === 'down' ? mid : vh - mid) - edge - TAIL_MID);
  const style = {
    [x === 'right' ? 'left' : 'right']: x === 'right' ? at.right + GAP : vw - at.left + GAP,
    [y === 'down' ? 'top' : 'bottom']: edge,
    width: Math.min(want, room[x]),
    maxHeight: vh - edge - MARGIN,
    transformOrigin: `${x === 'right' ? `${-TAIL_REACH}px` : `calc(100% + ${TAIL_REACH}px)`} ${
      y === 'down' ? `${tail + TAIL_MID}px` : `calc(100% - ${tail + TAIL_MID}px)`
    }`,
    '--tail-at': `${tail}px`,
  } as CSSProperties;
  return { x, y, style };
};

const FOCUSABLE = 'a[href], button, input, textarea, select, [tabindex]:not([tabindex="-1"])';

/**
 * A highlight zoomed out of its card: in a page-level layer over the dimmed
 * Fleet, the speaker's face stays where it is on the card (`face`, drawn over
 * it at `at`) and the bubble grows out of it (`growFrom`), then shrinks back
 * into it. Esc, a click outside or on the panel closes it; focus returns to
 * `origin`.
 */
const FleetZoom = ({
  origin,
  at,
  side,
  face,
  label,
  onClosed,
  children,
}: {
  origin: HTMLElement;
  /** The face's rectangle, or the tail's for words with no face. */
  at: DOMRect;
  /** The side of `at` the bubble is on in the card. */
  side: 'left' | 'right';
  face?: ReactNode;
  label: string;
  onClosed: () => void;
  children: ReactNode;
}) => {
  const [grow] = useState(() => growFrom(at, side, window.innerWidth, window.innerHeight));
  const panelRef = useRef<HTMLDivElement | null>(null);
  const scrimRef = useRef<HTMLDivElement | null>(null);
  const closing = useRef(false);

  useLayoutEffect(() => {
    const panel = panelRef.current!;
    panel.focus({ preventScroll: true });
    if (!moves()) return;
    const scrim = scrimRef.current!;
    panel.dataset.phase = scrim.dataset.phase = 'from';
    void panel.offsetWidth;
    delete panel.dataset.phase;
    delete scrim.dataset.phase;
  }, []);

  const close = useCallback(() => {
    const panel = panelRef.current;
    if (closing.current || !panel) return;
    closing.current = true;
    const done = () => {
      if (origin.isConnected) origin.focus({ preventScroll: true });
      onClosed();
    };
    if (!moves()) return done();
    panel.dataset.phase = 'closing';
    scrimRef.current!.dataset.phase = 'closing';
    window.setTimeout(done, CLOSE_MS);
  }, [origin, onClosed]);

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
    if (e.key !== 'Tab') return;
    const order = [e.currentTarget, ...e.currentTarget.querySelectorAll<HTMLElement>(FOCUSABLE)];
    const at = Math.max(0, order.indexOf(document.activeElement as HTMLElement));
    e.preventDefault();
    order[(at + (e.shiftKey ? -1 : 1) + order.length) % order.length]!.focus();
  };

  return createPortal(
    <div className={styles.zoomLayer}>
      <div ref={scrimRef} className={styles.zoomScrim} onClick={close} aria-hidden="true" />
      {face && (
        <span className={styles.zoomFace} style={{ left: at.left, top: at.top, width: at.width, height: at.height }} aria-hidden="true">
          {face}
        </span>
      )}
      <div
        ref={panelRef}
        className={styles.zoomPanel}
        data-x={grow.x}
        data-y={grow.y}
        style={grow.style}
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
        {children}
      </div>
    </div>,
    document.body
  );
};

export default FleetZoom;
