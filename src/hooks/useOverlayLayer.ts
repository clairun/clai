import { useEffect, useRef } from 'react';

/**
 * Registers a body-portaled overlay while it is open, so the app can answer
 * the two questions no single overlay can answer alone.
 *
 * Escape must dismiss only the topmost one. These overlays portal to `<body>`
 * and listen on `document`, so one keypress reaches all of them at once and
 * nothing in the DOM says which is on top — with two stacked (global Settings
 * over workspace Settings, a form modal over Settings, a dropdown over
 * either) a plain listener in each closes both. Overlays register here in the
 * order they open, which is the order they stack: one opened from inside
 * another opens later. Only the last registered acts on the key.
 *
 * Body scroll must stay locked until the last *modal* closes. Each modal used
 * to restore `overflow` on its own close, which unlocked the page behind a
 * modal that was still open. A non-modal layer passes `lockScroll: false`: it
 * takes its turn at Escape without freezing the page under it.
 *
 * This orders its members among themselves and nothing else, so an overlay
 * that can meet another on screen has to join. Current members, bottom to
 * top: WorkspaceSettingsModal, SettingsModal, McpServerFormModal and
 * AssistantProviderSettings' picker/form, IntervalSelect's popover — the
 * `--z-portal-*` layers in theme-light.css up to `--z-portal-popover`.
 * Above them, the lightbox and ConfirmDialog/ProgressDialog still keep their
 * own `document` listeners; they are reachable only from surfaces no member
 * covers, and an Escape that closes one of those closes a member underneath
 * too.
 */
const openOverlays: object[] = [];
const scrollLocks: object[] = [];

/** Drop a token wherever it sits. `splice(-1, 1)` would evict the topmost. */
const release = (stack: object[], token: object): void => {
  const at = stack.indexOf(token);
  if (at >= 0) stack.splice(at, 1);
};

export interface OverlayLayerOptions {
  /** Default true. False for a dropdown: it orders Escape, it is not a modal. */
  lockScroll?: boolean;
}

export const useOverlayLayer = (
  active: boolean,
  onEscape: () => void,
  { lockScroll = true }: OverlayLayerOptions = {}
): void => {
  // Kept in a ref so a fresh handler identity does not re-register the
  // overlay: that would move it back to the top of the stack while something
  // else is stacked above it.
  const handler = useRef(onEscape);
  useEffect(() => {
    handler.current = onEscape;
  }, [onEscape]);

  useEffect(() => {
    if (!active) return undefined;
    const token = {};
    openOverlays.push(token);
    if (lockScroll) {
      scrollLocks.push(token);
      document.body.style.overflow = 'hidden';
    }

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      if (openOverlays[openOverlays.length - 1] !== token) return;
      handler.current();
    };
    document.addEventListener('keydown', onKeyDown);

    return () => {
      document.removeEventListener('keydown', onKeyDown);
      release(openOverlays, token);
      release(scrollLocks, token);
      // This hook is the only writer of `overflow`, so there is never an
      // outside value for the last member out to preserve.
      if (scrollLocks.length === 0) document.body.style.overflow = '';
    };
  }, [active, lockScroll]);
};
