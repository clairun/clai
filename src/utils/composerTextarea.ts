/**
 * Sizing logic for the auto-growing composer textarea.
 *
 * Kept as a pure function so the grow-then-scroll behaviour is unit-testable
 * without a DOM (jsdom does not compute `scrollHeight`). The effect in
 * TerminalEmulator measures `scrollHeight` and applies the result to inline
 * styles.
 *
 * Behaviour: the textarea grows with its content up to `maxHeight`, then caps
 * and switches to internal scrolling (`overflow-y: auto`) so long multi-line
 * input stays reachable instead of being silently clipped. Below the cap the
 * scrollbar is hidden to avoid flicker on a single line.
 */
export interface TextareaSize {
  /** Rendered height in px to apply to `style.height`. */
  height: number;
  /** Whether the textarea should scroll internally. */
  overflowY: 'auto' | 'hidden';
}

export function computeTextareaSize(
  scrollHeight: number,
  lineHeight: number,
  maxHeight: number,
): TextareaSize {
  // `scrollHeight` is measured after collapsing to one line, so it never
  // reports less than a single line; clamp defensively anyway.
  const contentHeight = Math.max(scrollHeight, lineHeight);
  const height = Math.min(contentHeight, maxHeight);
  const overflowY = contentHeight > maxHeight ? 'auto' : 'hidden';
  return { height, overflowY };
}
