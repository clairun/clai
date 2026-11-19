import { beforeEach, describe, expect, it, vi } from 'vitest';

const mockAsk = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/plugin-dialog', () => ({ ask: mockAsk }));

import { askOnce } from './askOnce';

const pending = () => {
  let answer!: (value: boolean) => void;
  let fail!: (error: Error) => void;
  mockAsk.mockReturnValueOnce(new Promise<boolean>((resolve, reject) => { answer = resolve; fail = reject; }));
  return { answer: (value: boolean) => answer(value), fail: (error: Error) => fail(error) };
};

describe('askOnce', () => {
  beforeEach(() => {
    mockAsk.mockReset();
  });

  it('answers a second call with false while the first dialog is open', async () => {
    const dialog = pending();
    const first = askOnce('Delete?', { title: 'Delete' });

    await expect(askOnce('Delete?')).resolves.toBe(false);
    expect(mockAsk).toHaveBeenCalledTimes(1);

    dialog.answer(true);
    await expect(first).resolves.toBe(true);
  });

  it('asks again once the first dialog is answered', async () => {
    const dialog = pending();
    const first = askOnce('Delete?');
    dialog.answer(false);
    await first;

    mockAsk.mockResolvedValueOnce(true);
    await expect(askOnce('Delete?')).resolves.toBe(true);
    expect(mockAsk).toHaveBeenCalledTimes(2);
  });

  it('asks again after a dialog fails', async () => {
    const dialog = pending();
    const first = askOnce('Delete?');
    dialog.fail(new Error('no window'));
    await expect(first).rejects.toThrow('no window');

    mockAsk.mockResolvedValueOnce(true);
    await expect(askOnce('Delete?')).resolves.toBe(true);
  });
});
