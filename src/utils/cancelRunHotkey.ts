/**
 * Decision logic for the "Ctrl+C cancels the in-flight run" shortcut.
 *
 * Kept as a pure function so the guards are unit-testable without a DOM: the
 * effect that listens for keydown gathers the runtime facts (selection,
 * event target) and delegates the decision here.
 *
 * Guards, in order:
 * - only acts while a run is actually in flight;
 * - requires Ctrl (not Cmd/Alt/Shift) + the C key — so macOS Cmd+C still
 *   copies and we don't fire on Ctrl+Shift+C etc.;
 * - never steals a real text selection's copy;
 * - never steals the integrated terminal's own Ctrl+C (it must reach the PTY
 *   as SIGINT).
 */
export interface CancelHotkeyContext {
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  key: string;
  /** A run is currently in flight (cancellable). */
  hasActiveRun: boolean;
  /** The user has a non-empty text selection (Ctrl+C should copy). */
  hasTextSelection: boolean;
  /** The key event originated inside the integrated terminal (xterm). */
  targetInTerminal: boolean;
}

export function shouldCancelRunOnKey(ctx: CancelHotkeyContext): boolean {
  if (!ctx.hasActiveRun) return false;
  if (!ctx.ctrlKey || ctx.metaKey || ctx.altKey || ctx.shiftKey) return false;
  if (ctx.key !== 'c' && ctx.key !== 'C') return false;
  if (ctx.hasTextSelection) return false;
  if (ctx.targetInTerminal) return false;
  return true;
}
