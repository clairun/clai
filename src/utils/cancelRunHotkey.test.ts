import { describe, it, expect } from 'vitest';
import { shouldCancelRunOnKey, type CancelHotkeyContext } from './cancelRunHotkey';

const base: CancelHotkeyContext = {
  ctrlKey: true,
  metaKey: false,
  altKey: false,
  shiftKey: false,
  key: 'c',
  hasActiveRun: true,
  hasTextSelection: false,
  targetInTerminal: false,
};

describe('shouldCancelRunOnKey', () => {
  it('cancels on plain Ctrl+C while a run is active', () => {
    expect(shouldCancelRunOnKey(base)).toBe(true);
    expect(shouldCancelRunOnKey({ ...base, key: 'C' })).toBe(true);
  });

  it('does nothing when no run is in flight', () => {
    expect(shouldCancelRunOnKey({ ...base, hasActiveRun: false })).toBe(false);
  });

  it('lets a text selection copy instead of cancelling', () => {
    expect(shouldCancelRunOnKey({ ...base, hasTextSelection: true })).toBe(false);
  });

  it('lets the integrated terminal keep its own Ctrl+C (SIGINT)', () => {
    expect(shouldCancelRunOnKey({ ...base, targetInTerminal: true })).toBe(false);
  });

  it('ignores macOS Cmd+C (copy) and other modifier combos', () => {
    expect(shouldCancelRunOnKey({ ...base, ctrlKey: false, metaKey: true })).toBe(false);
    expect(shouldCancelRunOnKey({ ...base, altKey: true })).toBe(false);
    expect(shouldCancelRunOnKey({ ...base, shiftKey: true })).toBe(false);
  });

  it('only fires on the C key', () => {
    expect(shouldCancelRunOnKey({ ...base, key: 'v' })).toBe(false);
    expect(shouldCancelRunOnKey({ ...base, key: 'Escape' })).toBe(false);
  });
});
