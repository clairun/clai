import { useCtrlShiftShortcut } from '../hooks/useCtrlShiftShortcut';

/**
 * Ctrl+Shift+M (Cmd on macOS) switches between the Fleet and Workspace mode;
 * `onToggle` says where to.
 */
export const useFleetToggle = (onToggle: (() => void) | null) => useCtrlShiftShortcut('KeyM', onToggle);
