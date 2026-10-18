import { useCallback, useEffect, useLayoutEffect, useRef, type KeyboardEvent, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import styles from './FleetPage.module.css';

const CLOSE_MS = 250;

const moves = () =>
  typeof window.matchMedia === 'function' && !window.matchMedia('(prefers-reduced-motion: reduce)').matches;

/** The transform that lays `panel` over `origin`: same centre, scaled to its width. */
const over = (panel: HTMLElement, origin: HTMLElement): string | null => {
  if (!origin.isConnected) return null;
  const o = origin.getBoundingClientRect();
  const p = panel.getBoundingClientRect();
  if (!p.width || !o.width) return null;
  const dx = o.left + o.width / 2 - (p.left + p.width / 2);
  const dy = o.top + o.height / 2 - (p.top + p.height / 2);
  return `translate(${dx}px, ${dy}px) scale(${o.width / p.width})`;
};

const FOCUSABLE = 'a[href], button, input, textarea, select, [tabindex]:not([tabindex="-1"])';

/**
 * A highlight zoomed out of its card: a copy in a page-level layer that grows
 * from the bubble's rectangle to a centred panel, the rest of the Fleet dimmed,
 * and shrinks back along the same path. Esc, a click outside or on the panel
 * closes it; focus returns to `origin`.
 */
const FleetZoom = ({
  origin,
  label,
  onClosed,
  children,
}: {
  origin: HTMLElement;
  label: string;
  onClosed: () => void;
  children: ReactNode;
}) => {
  const panelRef = useRef<HTMLDivElement | null>(null);
  const scrimRef = useRef<HTMLDivElement | null>(null);
  const closing = useRef(false);

  useLayoutEffect(() => {
    const panel = panelRef.current!;
    panel.focus({ preventScroll: true });
    const from = moves() ? over(panel, origin) : null;
    if (!from) return;
    const scrim = scrimRef.current!;
    panel.dataset.phase = scrim.dataset.phase = 'from';
    panel.style.transform = from;
    void panel.offsetWidth;
    delete panel.dataset.phase;
    delete scrim.dataset.phase;
    panel.style.transform = '';
  }, [origin]);

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
    panel.style.transform = over(panel, origin) ?? 'scale(0.9)';
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
      <div
        ref={panelRef}
        className={styles.zoomPanel}
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
