import { useEffect } from 'react';
import { COLLAPSED_AGENT_QUESTION } from '../components/AskUserPanel/AskUserPanel';
import { dispatchFocusTerminal } from '../utils/workspaceUiEvents';

/** Open layers that own the keyboard even when focus has fallen to <body>; an expanded agent question is one. */
const OPEN_LAYER = `[role="dialog"]:not(${COLLAPSED_AGENT_QUESTION}), [role="alertdialog"], [role="menu"]`;

/** Nothing holds the keyboard: focus is on <body> or a collapsed agent question, and no layer is open. */
const keyboardIsFree = (): boolean => {
  const active = document.activeElement;
  if (active && active !== document.body && !active.closest(COLLAPSED_AGENT_QUESTION)) return false;
  return !document.querySelector(OPEN_LAYER);
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
 * Puts the keyboard in the composer while the workspace layout is shown and
 * nothing else has it: on entering a workspace, and on the first character
 * typed, which also covers the workspace index where `workspaceId` is null.
 * The keydown is not prevented: WebKit looks up the focused element again
 * before keypress, so the character lands in the composer.
 */
export function useComposerTypeToFocus(workspaceId: string | null): void {
  useEffect(() => {
    if (workspaceId && keyboardIsFree()) dispatchFocusTerminal();
  }, [workspaceId]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing || !typesCharacter(e)) return;
      if (keyboardIsFree()) dispatchFocusTerminal();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);
}
