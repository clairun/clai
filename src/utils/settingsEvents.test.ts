import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  emit: vi.fn(async (_event: string, _source: string) => {}),
  handlers: new Map<string, (event: { payload: string }) => void>(),
}));
vi.mock('@tauri-apps/api/event', () => ({
  emit: mocks.emit,
  listen: vi.fn(async (event: string, handler: (event: { payload: string }) => void) => {
    mocks.handlers.set(event, handler);
    return () => { mocks.handlers.delete(event); };
  }),
}));

const { listenForSettingsChanges, notifySettingsChanged } = await import('./settingsEvents');
const events = [
  'assistant-provider-connections-changed',
  'mcp-servers-changed',
  'workspace-settings-changed',
] as const;

afterEach(() => {
  mocks.handlers.clear();
  mocks.emit.mockClear();
});

describe('settings change signals', () => {
  it.each(events)('dispatches %s locally and forwards changes from another window', async (event) => {
    const received = vi.fn();
    window.addEventListener(event, received);
    const stop = listenForSettingsChanges();
    await vi.waitFor(() => expect(mocks.handlers.has(event)).toBe(true));
    notifySettingsChanged(event);
    expect(received).toHaveBeenCalledTimes(1);
    expect(mocks.emit).toHaveBeenCalledWith(event, expect.any(String));
    const source = mocks.emit.mock.calls.at(-1)![1];
    mocks.handlers.get(event)!({ payload: source });
    expect(received).toHaveBeenCalledTimes(1);
    mocks.handlers.get(event)!({ payload: 'other-window' });
    expect(received).toHaveBeenCalledTimes(2);
    stop();
    expect(mocks.handlers.has(event)).toBe(false);
    window.removeEventListener(event, received);
  });
});
