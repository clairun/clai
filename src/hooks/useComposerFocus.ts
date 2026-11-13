import { useEffect } from 'react';
import { COLLAPSED_AGENT_QUESTION } from '../components/AskUserPanel/AskUserPanel';
import { dispatchFocusTerminal } from '../utils/workspaceUiEvents';

/** Open layers that own the keyboard even when focus has fallen to <body>; an expanded agent question is one. */
const OPEN_LAYER = `[role="dialog"]:not(${COLLAPSED_AGENT_QUESTION}), [role="alertdialog"], [role="menu"]`;

const TEXT_FIELD = 'input, textarea, select, [contenteditable]:not([contenteditable="false"])';

const layerIsOpen = (): boolean => document.querySelector(OPEN_LAYER) !== null;

/** Nothing holds the keyboard: focus is on <body> or a collapsed agent question, and no layer is open. */
const keyboardIsFree = (): boolean => {
  const active = document.activeElement;
  if (active && active !== document.body && !active.closest(COLLAPSED_AGENT_QUESTION)) return false;
  return !layerIsOpen();
};

/**
 * The composer may take the keyboard back: no text field has it (the composer
 * itself included, which then keeps it) and no layer is open. Focus left on a
 * button, a row or <body>, or text selected in the chat, does not stop it.
 */
const composerMayReclaim = (): boolean => {
  if (document.activeElement?.matches(TEXT_FIELD)) return false;
  return !layerIsOpen();
};

/**
 * A key that types a character; Space is left to page the scroller under the
 * last click. Windows reports AltGr (as in AltGr+2 for @) as Ctrl+Alt.
 */
const typesCharacter = (e: KeyboardEvent): boolean =>
  !e.metaKey &&
  (e.getModifierState('AltGraph') || (!e.ctrlKey && !e.altKey)) &&
  e.key.length === 1 &&
  e.key !== ' ';

/**
 * Keeps the keyboard in the composer while the workspace layout is shown:
 * - on entering a workspace, when nothing else has it;
 * - when the window is activated, whatever click moved focus off it before;
 * - after the press that activates the window, when it leaves focus on <body>
 *   (WebKitGTK focuses a pressed button, link or [tabindex] element on
 *   mousedown, so <body> means plain content) and selects nothing;
 * - on the first character typed while nothing has the keyboard, which also
 *   covers the workspace index where `workspaceId` is null. The keydown is not
 *   prevented: WebKit looks up the focused element again before keypress, so
 *   the character lands in the composer.
 */
export function useComposerFocus(workspaceId: string | null): void {
  useEffect(() => {
    if (workspaceId && keyboardIsFree()) dispatchFocusTerminal();
  }, [workspaceId]);

  useEffect(() => {
    // A primary press is in progress, and whether the window was activated during it.
    let pressed = false;
    let armed = false;
    let pending: ReturnType<typeof setTimeout> | undefined;

    const reset = () => {
      pressed = false;
      armed = false;
    };
    const onPointerDown = (e: PointerEvent) => {
      pressed = e.button === 0;
    };
    // WebKitGTK delivers the activation during the press that causes it, so
    // that press's release decides; focusing the composer mid-press would take
    // the press's own focus and clamp a drag-select to the composer. Were the
    // activation to come before the press, a press on plain content would end
    // on <body>, and only typing would bring the composer back.
    const onFocus = () => {
      if (pressed) armed = true;
      else if (composerMayReclaim()) dispatchFocusTerminal();
    };
    const onPointerUp = () => {
      pressed = false;
      if (!armed) return;
      armed = false;
      // WebKit dispatches click in the same task as pointerup and mouseup, so
      // the next task runs after the click's own handlers.
      pending = setTimeout(() => {
        if (document.getSelection()?.isCollapsed !== false && keyboardIsFree()) dispatchFocusTerminal();
      });
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing || !typesCharacter(e)) return;
      if (keyboardIsFree()) dispatchFocusTerminal();
    };

    window.addEventListener('focus', onFocus);
    // A press released outside the window sends no pointerup; blur ends it.
    window.addEventListener('blur', reset);
    // Capture, so a handler that stops propagation cannot hide the press.
    window.addEventListener('pointerdown', onPointerDown, true);
    window.addEventListener('pointerup', onPointerUp, true);
    window.addEventListener('pointercancel', reset, true);
    window.addEventListener('keydown', onKeyDown);
    return () => {
      clearTimeout(pending);
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('blur', reset);
      window.removeEventListener('pointerdown', onPointerDown, true);
      window.removeEventListener('pointerup', onPointerUp, true);
      window.removeEventListener('pointercancel', reset, true);
      window.removeEventListener('keydown', onKeyDown);
    };
  }, []);
}
