import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { useEffect, useRef } from 'react';
import { MemoryRouter, Route, Routes, useParams } from 'react-router';
import { FOCUS_TERMINAL_EVENT } from '../utils/workspaceUiEvents';

import FleetPage from './FleetPage';
import WorkspacePicker from '../components/TerminalEmulator/WorkspacePicker';
import { useFleetStore } from '../fleet/fleetStore';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { useTaskStore } from '../workspace/taskStore';
import { EMPTY_FLEET, emptyCard, shortTime } from '../fleet/fleetModel';
import type { FleetWorkspace } from '../generated/bindings';

const mockInvoke = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

vi.mock('../fleet/client', () => ({ fleetSnapshot: vi.fn() }));
vi.mock('../api/client', () => ({ listAgentDefinitions: vi.fn(async () => []) }));
vi.mock('../components/AppUpdateBadge', () => ({ default: () => null }));
const { fleetSnapshot } = vi.mocked(await import('../fleet/client'));

const workspace = (workspaceId: string, title: string, ageMs = 0): FleetWorkspace => ({
  workspaceId,
  title,
  starred: false,
  lastActivityAt: BigInt(Date.now() - ageMs),
  lastOpenedAt: 0n,
  agents: [],
  mainSessionId: null,
  lastMainRun: null,
  activeRunIds: [],
  tasks: [],
  pendingPermissions: [],
  pendingPathGrants: [],
  pendingAsks: [],
  lastAssistantMessage: null,
  recentMessages: [],
});

const said = (messageId: string, text: string, ago: number, patch = {}) =>
  ({ messageId, sessionId: 'main-a', role: 'user', origin: { kind: 'user' }, text, truncated: false, createdAt: BigInt(Date.now() - ago), runId: null, ...patch }) as FleetWorkspace['recentMessages'][number];

/** The shared terminal's input, as far as the page sees it: it takes the keyboard when asked (`TerminalEmulator`). */
const FakeTerminal = () => {
  const ref = useRef<HTMLTextAreaElement | null>(null);
  useEffect(() => {
    const focus = () => ref.current?.focus();
    window.addEventListener(FOCUS_TERMINAL_EVENT, focus);
    return () => window.removeEventListener(FOCUS_TERMINAL_EVENT, focus);
  }, []);
  return <textarea ref={ref} aria-label="Terminal" />;
};

const renderFleet = () =>
  render(
    <MemoryRouter>
      <FleetPage />
      <FakeTerminal />
    </MemoryRouter>
  );

beforeEach(() => {
  useFleetStore.setState({ fleet: EMPTY_FLEET, selectedId: null, loadedAt: null, loadError: null, buffer: null });
  useFleetActivityStore.setState({ activeRunsByWorkspace: {} });
  useTaskStore.setState({ tasks: {}, seeded: {} });
  // A 4 × 3 grid.
  vi.stubGlobal(
    'ResizeObserver',
    class {
      constructor(private cb: ResizeObserverCallback) {}
      observe() {
        this.cb([{ contentRect: { width: 1400, height: 700 } } as ResizeObserverEntry], this as never);
      }
      disconnect() {}
    }
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  mockInvoke.mockImplementation(async () => undefined);
  fleetSnapshot.mockReset();
  mockInvoke.mockClear();
});

describe('FleetPage', () => {
  it('reloads the snapshot once for a card born untitled, and not again while it stays untitled', async () => {
    fleetSnapshot.mockResolvedValueOnce([]).mockResolvedValue([workspace('ws-new', '')]);
    renderFleet();
    await waitFor(() => expect(useFleetStore.getState().loadedAt).not.toBeNull());
    expect(fleetSnapshot).toHaveBeenCalledTimes(1);

    const bump = () =>
      act(() =>
        useFleetStore.getState().dispatch((s) => ({
          cards: { ...s.cards, 'ws-new': { ...(s.cards['ws-new'] ?? emptyCard('ws-new')), lastActivityAt: Date.now() + 1 } },
        }))
      );
    bump();
    await waitFor(() => expect(fleetSnapshot).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(useFleetStore.getState().buffer).toBeNull());
    bump();
    await act(async () => {});
    expect(fleetSnapshot).toHaveBeenCalledTimes(2);
  });

  it('opens the switcher on Ctrl+Tab, most recently opened first, moves only its highlight while Ctrl is held, and selects on release', async () => {
    const opened = (ago: number) => BigInt(Date.now() - ago);
    fleetSnapshot.mockResolvedValue([
      { ...workspace('a', 'Alpha'), lastOpenedAt: opened(3 * 60_000) },
      // Busiest, but never opened: after the opened ones.
      workspace('b', 'Beta'),
      { ...workspace('c', 'Gamma', 60_000), lastOpenedAt: opened(60_000) },
    ]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Alpha' });
    const terminal = screen.getByRole('textbox', { name: 'Terminal' });
    const press = (shiftKey = false) => fireEvent.keyDown(terminal, { key: 'Tab', code: 'Tab', ctrlKey: true, shiftKey });
    const entries = () => screen.getAllByRole('option').map((o) => o.textContent);
    const highlighted = () =>
      within(screen.getByRole('listbox', { name: 'Switch workspace' })).getByRole('option', { selected: true })
        .textContent;
    const slots = () => screen.getAllByRole('button', { name: /^Select / }).map((b) => b.textContent);
    const before = slots();

    expect(press()).toBe(false); // default (a tab, or a focus move) prevented
    expect(entries()).toEqual([
      expect.stringContaining('Gamma'),
      expect.stringContaining('Alpha'),
      expect.stringContaining('Beta'),
      'New workspace',
    ]);
    expect(highlighted()).toContain('Gamma');
    press();
    press(true);
    press();
    expect(highlighted()).toContain('Alpha');
    expect(useFleetStore.getState().selectedId).toBeNull();

    terminal.blur();
    fireEvent.keyUp(terminal, { key: 'Control' });
    expect(useFleetStore.getState().selectedId).toBe('a');
    // Typing goes to the chosen workspace at once.
    expect(terminal).toHaveFocus();
    expect(mockInvoke).toHaveBeenCalledWith('workspace_mark_opened', { workspaceId: 'a' });
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    // The same as a click: the card is highlighted, nothing moves.
    expect(slots()).toEqual(before);
    expect(screen.getByRole('button', { name: 'Select Alpha' }).closest('article')).toHaveAttribute('aria-current', 'true');

    // Opened now, it leads; the next one is the one opened before.
    press();
    expect(entries()[0]).toContain('Alpha');
    expect(highlighted()).toContain('Gamma');
    // Escape closes it with no change.
    fireEvent.keyDown(terminal, { key: 'Escape', ctrlKey: true });
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    fireEvent.keyUp(terminal, { key: 'Control' });
    expect(useFleetStore.getState().selectedId).toBe('a');

    // "New workspace", the last entry, deselects: the terminal then starts one.
    press(true);
    expect(highlighted()).toBe('New workspace');
    fireEvent.keyUp(terminal, { key: 'Control' });
    expect(useFleetStore.getState().selectedId).toBeNull();
  });

  it('goes backwards on Ctrl+Shift+Tab, wrapping from the first entry to the last; Enter chooses and readies the terminal', async () => {
    fleetSnapshot.mockResolvedValue([workspace('a', 'Alpha'), workspace('b', 'Beta')]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Alpha' });
    const terminal = screen.getByRole('textbox', { name: 'Terminal' });
    // WebKitGTK reports Shift+Tab (keysym ISO_Left_Tab) as key 'Unidentified'; only `code` is 'Tab'.
    const press = (shiftKey = false) =>
      fireEvent.keyDown(terminal, { key: shiftKey ? 'Unidentified' : 'Tab', code: 'Tab', ctrlKey: true, shiftKey });
    const highlighted = () => screen.getByRole('option', { selected: true }).textContent;
    press();
    expect(highlighted()).toContain(screen.getAllByRole('option')[0]!.textContent!);
    press(true);
    expect(highlighted()).toBe('New workspace');
    terminal.blur();
    fireEvent.keyDown(terminal, { key: 'Enter', ctrlKey: true });
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(useFleetStore.getState().selectedId).toBeNull();
    expect(terminal).toHaveFocus();
  });

  it('colours the border of a working card, but the selected one wins', async () => {
    fleetSnapshot.mockResolvedValue([
      { ...workspace('a', 'Alpha'), activeRunIds: ['run-1'] },
      { ...workspace('b', 'Beta'), activeRunIds: ['run-2'] },
    ]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Alpha' });
    const card = (name: string) => screen.getByRole('button', { name: `Select ${name}` }).closest('article')!;
    await waitFor(() => expect(card('Alpha').className).toMatch(/cardWorking/));
    act(() => useFleetActivityStore.getState().markRunEnded('a', 'run-1'));
    expect(card('Alpha').className).not.toMatch(/cardWorking/);

    act(() => useFleetStore.getState().select('b'));
    expect(card('Beta')).toHaveAttribute('aria-current', 'true');
    expect(card('Beta').className).toMatch(/cardSelected/);
    expect(card('Beta').className).not.toMatch(/cardWorking/);
  });

  it('colours the border of a card that needs the user, but the selected one wins', async () => {
    const ask = (workspaceId: string) => ({
      pendingId: `q-${workspaceId}`, workspaceId, sessionId: 'main', runId: 'r', question: 'Ship it?',
      options: null, multiSelect: false, extraContext: null, requestedAt: 4n,
    });
    fleetSnapshot.mockResolvedValue([
      { ...workspace('a', 'Alpha'), activeRunIds: ['run-1'], pendingAsks: [ask('a')] },
      { ...workspace('b', 'Beta'), pendingAsks: [ask('b')] },
    ]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Alpha' });
    const card = (name: string) => screen.getByRole('button', { name: `Select ${name}` }).closest('article')!;
    // Waiting outranks working.
    await waitFor(() => expect(card('Alpha').className).toMatch(/cardWaiting/));
    expect(card('Alpha').className).not.toMatch(/cardWorking/);

    act(() => useFleetStore.getState().select('b'));
    expect(card('Beta').className).toMatch(/cardSelected/);
    expect(card('Beta').className).not.toMatch(/cardWaiting/);
  });

  it('draws loud only the card that needs you, never the selected one: burst, sticker and "!?"', async () => {
    const ask = (workspaceId: string) => ({
      pendingId: `q-${workspaceId}`, workspaceId, sessionId: 'main', runId: 'r', question: 'Ship it?',
      options: null, multiSelect: false, extraContext: null, requestedAt: 4n,
    });
    const running = { id: 't', title: 'Crawl', status: 'running', parentTaskId: null, createdAt: BigInt(Date.now()), updatedAt: BigInt(Date.now()), assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' } as never;
    fleetSnapshot.mockResolvedValue([
      workspace('s', 'Staged'),
      { ...workspace('a', 'Alpha', 10), pendingAsks: [ask('a')] },
      { ...workspace('w', 'Worker', 20), tasks: [running] },
    ]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Alpha' });
    const card = (name: string) => screen.getByRole('button', { name: `Select ${name}` }).closest('article')!;
    await waitFor(() => expect(card('Alpha')).toHaveAttribute('data-loud', 'needs'));
    expect(within(card('Alpha')).getByText('!?')).toHaveAttribute('aria-hidden', 'true');
    // The sticker is decoration; the phase still reads in full.
    expect(within(card('Alpha')).getByText('Needs you')).toHaveAttribute('aria-hidden', 'true');
    expect(within(card('Alpha')).getByText('Needs you · 1 question')).toBeInTheDocument();
    // A running card tilts and shouts "GO!", but is not the loud one.
    expect(card('Worker')).not.toHaveAttribute('data-loud');
    expect(within(card('Worker')).getByText('GO!')).toBeInTheDocument();
    expect(document.querySelectorAll('[data-loud]')).toHaveLength(1);

    act(() => useFleetStore.getState().select('a'));
    expect(card('Alpha')).not.toHaveAttribute('data-loud');
  });

  it('draws loud every card that needs you and nothing else; only the newest running task rushes in with "GO!"', async () => {
    const ask = { pendingId: 'q', workspaceId: 'a', sessionId: 'main', runId: 'r', question: 'Ship it?', options: null, multiSelect: false, extraContext: null, requestedAt: 4n };
    const task = (id: string, status: string, ago: number) =>
      ({ id, title: `Task ${id}`, status, resultSummary: `Result ${id}.`, parentTaskId: null, createdAt: BigInt(Date.now() - ago), updatedAt: BigInt(Date.now() - ago), completedAt: status === 'running' ? null : BigInt(Date.now() - ago), assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' }) as never;
    fleetSnapshot.mockResolvedValue([
      { ...workspace('r1', 'Run one', 10), tasks: [task('r1', 'running', 3 * 60_000)] },
      { ...workspace('r2', 'Run two', 20), tasks: [task('r2', 'running', 60_000)] },
      { ...workspace('r3', 'Run three', 30), tasks: [task('r3', 'running', 2 * 60_000)] },
      { ...workspace('f1', 'Done one', 40), tasks: [task('f1', 'completed', 60_000)] },
      { ...workspace('f2', 'Done two', 50), tasks: [task('f2', 'completed', 2 * 60_000)] },
      { ...workspace('a', 'Asking', 60), pendingAsks: [ask] },
    ]);
    renderFleet();
    await screen.findByText('Result f2.');
    const card = (name: string) => screen.getByRole('button', { name: `Select ${name}` }).closest('article')!;
    expect([...document.querySelectorAll('[data-loud]')]).toEqual([card('Asking')]);
    expect(screen.getAllByText(/^GO/).map((go) => go.closest('article'))).toEqual([card('Run two')]);
    expect(screen.queryByText('DONE!')).not.toBeInTheDocument();
    // Every running card still thinks.
    for (const name of ['Run one', 'Run two', 'Run three']) expect(within(card(name)).getByRole('img', { name: 'Scout is working' })).toBeInTheDocument();
  });

  it('lets a finished task stop cheering on the clock, with no further events', async () => {
    vi.useFakeTimers({ now: Date.now(), toFake: ['Date', 'setInterval', 'clearInterval'] });
    try {
      const done = { id: 't-clock', title: 'Fix it', status: 'completed', resultSummary: 'Fixed it.', parentTaskId: null, updatedAt: BigInt(Date.now()), completedAt: BigInt(Date.now()), assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' } as never;
      fleetSnapshot.mockResolvedValue([workspace('staged', 'Staged'), { ...workspace('clock', 'Alpha', 1000), tasks: [done] }]);
      renderFleet();
      const card = (await screen.findByText('Fixed it.')).closest('article')!;
      expect(card).toHaveAttribute('data-loud', 'done');
      expect(within(card).getByText('DONE!')).toBeInTheDocument();
      expect(within(card).getByText('Just now')).toBeInTheDocument();
      act(() => vi.advanceTimersByTime(15 * 60_000));
      expect(card).not.toHaveAttribute('data-loud');
      expect(within(card).queryByText('DONE!')).not.toBeInTheDocument();
      expect(within(card).getByText('Last word · 15 min ago')).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it('fills the card with bubbles from the newest; two older ones collapse to rows; Main speaks to you unlabelled', async () => {
    // A scene 300 high with a 20-high caption; every bubble 62 high (70 with its gap).
    const height = (el: HTMLElement) => (el.hasAttribute('data-row') ? 62 : el.tagName === 'P' ? 20 : 0);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return height(this);
    });
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.className.includes('scene') ? 300 : 0;
    });
    const at = (ago: number) => BigInt(Date.now() - ago);
    const task = (id: string, title: string, status: string, ago: number, extra = {}) =>
      ({ id, title, status, parentTaskId: null, createdAt: at(ago), updatedAt: at(ago), completedAt: status === 'running' ? null : at(ago - 1000), assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s', ...extra }) as never;
    fleetSnapshot.mockResolvedValue([
      {
        ...workspace('a', 'Alpha'),
        mainSessionId: 'main-a',
        tasks: [task('t1', 'Old research', 'running', 35 * 60_000), task('t2', 'Fix the build', 'failed', 40 * 60_000, { error: 'Cargo ran out of disk. Free 2 GB.' })],
        recentMessages: [
          said('u1', 'Look into the leaks. All of them.', 60 * 60_000),
          said('u2', 'Then fix the build', 45 * 60_000),
          said('a1', 'On it. Two tasks out.', 30 * 60_000, { role: 'assistant' }),
          said('u3', 'Thanks!', 20 * 60_000),
          said('a2', 'Shipped **v2**', 10 * 60_000, { role: 'assistant' }),
        ],
      },
    ]);
    renderFleet();
    const card = within((await screen.findByText('v2')).closest('article')!);
    // Three bubbles fit, newest last: Main's, yours, Main's.
    const bubbles = card.getAllByRole('button', { name: /On it|Thanks|Shipped/ });
    const stamp = (b: HTMLElement) => b.querySelector('[class*=stamp]')!.textContent;
    const words = (b: HTMLElement) => b.querySelector('[data-bubble]')!.textContent!.replace(stamp(b)!, '');
    expect(bubbles.map(words)).toEqual(['On it. Two tasks out.', 'Thanks!', 'Shipped v2']);
    // Two rows of what came before; older ones hide.
    const rows = within(card.getByRole('list', { name: 'Earlier' }));
    expect(rows.getAllByRole('listitem').map((row) => row.textContent)).toEqual([
      expect.stringMatching(/^Scout → Main Cargo ran out of disk\.failed ·/),
      expect.stringMatching(/^Main → Scout Old research\s*working ·/),
    ]);
    expect(card.queryByText(/Look into the leaks/)).not.toBeInTheDocument();
    // The running task is in the crew strip, whichever rows fit: one rule.
    expect(card.getByRole('button', { name: 'Scout working on: Old research' })).toBeInTheDocument();
    expect(rows.getAllByRole('listitem')[1]!.querySelector('[class*=rowThinking]')).toBeNull();
    // Each bubble starts with its time; no line under it. Main is named once,
    // under its face; your word to Main needs no label at all.
    const now = Date.now();
    expect(bubbles.map(stamp)).toEqual([30, 20, 10].map((min) => `${shortTime(now - min * 60_000, now)} · `));
    expect(card.queryByText('you')).not.toBeInTheDocument();
    expect(bubbles[0]!.textContent!.match(/Main/g)).toHaveLength(1);
    expect(bubbles[0]!.querySelector('figcaption')).toHaveTextContent('Main');
    expect(bubbles[1]!.textContent).not.toMatch(/You|Main/);
    expect(card.getByText(/^Now/)).toBeInTheDocument();
  });

  it('names the task a word of yours went to, in the time token', async () => {
    const t = { id: 't1', title: 'Harvest W12', status: 'running', parentTaskId: null, sessionId: 'task-s1', createdAt: BigInt(Date.now() - 60_000), updatedAt: BigInt(Date.now() - 60_000), assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' } as never;
    fleetSnapshot.mockResolvedValue([
      // Its own workspace: the task store outlives each test.
      { ...workspace('tw', 'Tasked'), mainSessionId: 'main-t', tasks: [t], recentMessages: [said('u1', 'Faster, please.', 30_000, { sessionId: 'task-s1' })] },
    ]);
    renderFleet();
    const bubble = (await screen.findByText('Faster, please.')).closest<HTMLElement>('[role=button]')!;
    expect(bubble.querySelector('[title] > span:first-child')).toHaveTextContent(/^\S+( \S+)? → Scout ·$/);
  });

  describe('zoom', () => {
    // An agent's word to Main: Main's own bubbles open in place instead (`FleetFeed.test`).
    const scout = { kind: 'agent', agent: { workspaceAgentId: 'wa-s', agentDefinitionId: 'def-s', displayName: 'Scout', isDefault: false }, sessionId: 'task-s', runId: null, replyAddress: null };
    const seed = (patch: Partial<FleetWorkspace['recentMessages'][number]> = {}) =>
      fleetSnapshot.mockResolvedValue([
        workspace('s', 'Staged'),
        { ...workspace('a', 'Alpha', 1000), mainSessionId: 'main-a', recentMessages: [said('u1', 'Ship it.', 20 * 60_000), said('a2', 'Shipped **v2** to staging', 10 * 60_000, { origin: scout, ...patch })] },
      ]);
    const bubble = async (text: RegExp) => (await screen.findByText(text)).closest<HTMLElement>('[role=button]')!;

    it('zooms a bubble out to the whole message as markdown; Esc zooms back and focus returns to it', async () => {
      seed();
      renderFleet();
      const origin = await bubble(/^Shipped v2/);
      fireEvent.click(origin);
      const dialog = screen.getByRole('dialog', { name: 'Message from Scout' });
      expect(within(dialog).getByText('v2').tagName).toBe('STRONG');
      // The zoom says who to whom and when, in full, before the words.
      expect(dialog.querySelector('p')).toHaveTextContent(/^Scout → Main · \S/);
      expect(dialog).toHaveFocus();
      // Zooming is not selecting the card.
      expect(useFleetStore.getState().selectedId).toBeNull();
      expect(mockInvoke).not.toHaveBeenCalledWith('assistant_get_message', expect.anything());

      fireEvent.keyDown(window, { key: 'Escape' });
      expect(screen.queryByRole('dialog', { name: 'Message from Scout' })).not.toBeInTheDocument();
      expect(origin).toHaveFocus();

      // From the keyboard too; a click on the panel zooms back.
      fireEvent.keyDown(origin, { key: 'Enter' });
      fireEvent.click(screen.getByRole('dialog', { name: 'Message from Scout' }));
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });

    /** The card's bubbles and tails at fixed places: a bubble at `bubble`, its tail 20px down its side. */
    const layout = (bubble: { left: number; top: number; width: number; height: number }) =>
      vi.spyOn(Element.prototype, 'getBoundingClientRect').mockImplementation(function (this: Element) {
        const at = this.hasAttribute('data-bubble')
          ? bubble
          : this.hasAttribute('data-tail') && this.closest('[data-bubble]')
            ? { left: this.getAttribute('class')!.includes('tailRight') ? bubble.left + bubble.width - 2 : bubble.left - 22, top: bubble.top + 20, width: 24, height: 22 }
            : this.hasAttribute('data-anchor')
              ? { left: bubble.left - 52, top: bubble.top + 14, width: 36, height: 36 }
              : { left: 0, top: 0, width: 0, height: 0 };
        return { ...at, x: at.left, y: at.top, right: at.left + at.width, bottom: at.top + at.height, toJSON: () => at } as DOMRect;
      });

    it('starts as the card bubble, covers it as it grows, keeps the tail on the face, and hides the original while open', async () => {
      layout({ left: 152, top: 190, width: 300, height: 60 });
      seed();
      renderFleet();
      const origin = await bubble(/^Shipped v2/);
      const source = origin.querySelector<HTMLElement>('[data-bubble]')!;
      fireEvent.click(origin);
      const dialog = screen.getByRole('dialog', { name: 'Message from Scout' });
      // jsdom measures no content, so the zoom ends no taller than the bubble: same edges but the far one.
      expect(dialog.style).toMatchObject({ left: '152px', top: '190px', height: '60px', width: `${Math.min(560, 1024 * 0.6)}px` });
      expect(dialog.querySelector<HTMLElement>('[data-tail]')!.style.top).toBe('20px');
      expect(dialog).toHaveAttribute('data-tail-side', 'left');
      // One bubble at a time: the card's hides, keeping its place, and comes back.
      expect(source.style.visibility).toBe('hidden');
      // An agent's bubble is the one agent neutral: no agent colour on it, nor on its zoom.
      expect(source.style.getPropertyValue('--agent-color')).toBe('');
      expect(dialog.style.getPropertyValue('--agent-color')).toBe('');
      expect(dialog.style.getPropertyValue('--bubble-border')).not.toContain('fleet-you-bubble');
      fireEvent.keyDown(window, { key: 'Escape' });
      expect(source.style.visibility).toBe('');
    });

    it("takes your bubble's neutral, not an agent's", async () => {
      layout({ left: 600, top: 500, width: 200, height: 40 });
      // Room for both bubbles.
      vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.hasAttribute('data-row') ? 60 : 0;
      });
      vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.className.includes('scene') ? 400 : 0;
      });
      seed();
      renderFleet();
      const origin = await bubble(/^Ship it\./);
      fireEvent.click(origin);
      const dialog = screen.getByRole('dialog', { name: 'Message from You' });
      expect(origin.style.getPropertyValue('--agent-color')).toBe('');
      expect(dialog.style.getPropertyValue('--agent-color')).toBe('');
      expect(dialog).toHaveAttribute('data-tail-side', 'right');
      expect(document.querySelector('[data-tone]')).toBeNull();
    });

    it('zooms a collapsed row too', async () => {
      vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.hasAttribute('data-row') ? 100 : 0;
      });
      vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.className.includes('scene') ? 160 : 0;
      });
      seed();
      renderFleet();
      const row = within(await screen.findByRole('list', { name: 'Earlier' })).getByRole('button');
      fireEvent.click(row);
      const dialog = screen.getByRole('dialog', { name: 'Message from You' });
      expect(within(dialog).getByText('Ship it.')).toBeInTheDocument();
      // No bubble to copy, yet still your neutral.
      expect(dialog.style.getPropertyValue('--bubble-border')).toBe('var(--fleet-you-bubble)');
    });

    it("zooms a row of Main's words as markdown, its chart a chip", async () => {
      vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.hasAttribute('data-row') ? 100 : 0;
      });
      vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.className.includes('scene') ? 160 : 0;
      });
      fleetSnapshot.mockResolvedValue([
        {
          ...workspace('a', 'Alpha', 1000),
          mainSessionId: 'main-a',
          recentMessages: [said('a1', 'Leaks:\n\n- by day: ![Leaks by day](charts/leaks.vl.json)', 20 * 60_000, { role: 'assistant' }), said('u2', 'Thanks', 10 * 60_000)],
        },
      ]);
      renderFleet();
      fireEvent.click(within(await screen.findByRole('list', { name: 'Earlier' })).getByRole('button'));
      const dialog = screen.getByRole('dialog', { name: 'Message from Main' });
      const chip = within(dialog).getByRole('button', { name: /Leaks by day/ });
      expect(chip.closest('li')).toHaveTextContent('by day: Leaks by day');
      expect(dialog.querySelector('canvas, svg[role=graphics-document]')).toBeNull();
    });

    it('shows a cut message at once and swaps in the whole one when it arrives', async () => {
      seed({ text: 'Shipped **v2** to', truncated: true });
      let resolve: (m: unknown) => void = () => {};
      mockInvoke.mockImplementation(((cmd: string) => (cmd === 'assistant_get_message' ? new Promise((r) => (resolve = r)) : Promise.resolve(undefined))) as never);
      renderFleet();
      fireEvent.click(await bubble(/^Shipped v2 to…/));
      const dialog = screen.getByRole('dialog', { name: 'Message from Scout' });
      expect(dialog).toHaveTextContent('Shipped v2 to…');
      expect(mockInvoke).toHaveBeenCalledWith('assistant_get_message', { workspaceId: 'a', sessionId: 'main-a', messageId: 'a2' });
      await act(async () => resolve({ id: 'a2', sessionId: 'main-a', role: 'assistant', content: [{ type: 'text', text: 'Shipped **v2** to staging and prod.' }] }));
      expect(dialog).toHaveTextContent('Shipped v2 to staging and prod.');
    });

    it('keeps the cut message when the whole one cannot be fetched', async () => {
      seed({ text: 'Shipped **v2** to', truncated: true });
      mockInvoke.mockImplementation(((cmd: string) => (cmd === 'assistant_get_message' ? Promise.reject(new Error('Assistant message not found: a2')) : Promise.resolve(undefined))) as never);
      renderFleet();
      fireEvent.click(await bubble(/^Shipped v2 to…/));
      await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('assistant_get_message', expect.anything()));
      await act(async () => {});
      expect(screen.getByRole('dialog', { name: 'Message from Scout' })).toHaveTextContent('Shipped v2 to…');
    });
  });

  it('deselects on Escape, from the terminal too, and on a click on the empty board; nothing moves', async () => {
    fleetSnapshot.mockResolvedValue([workspace('s', 'Staged'), workspace('a', 'Alpha', 1000)]);
    renderFleet();
    fireEvent.click(await screen.findByRole('button', { name: 'Select Alpha' }));
    expect(useFleetStore.getState().selectedId).toBe('a');
    expect(screen.getAllByRole('button', { name: /^Select / }).map((b) => b.textContent)).toEqual(['Staged', 'Alpha']);
    fireEvent.keyDown(screen.getByRole('textbox', { name: 'Terminal' }), { key: 'Escape' });
    expect(useFleetStore.getState().selectedId).toBeNull();
    expect(screen.getAllByRole('button', { name: /^Select / }).map((b) => b.textContent)).toEqual(['Staged', 'Alpha']);

    fireEvent.click(screen.getByRole('button', { name: 'Select Alpha' }));
    fireEvent.click(screen.getAllByRole('article')[0]!.parentElement!);
    expect(useFleetStore.getState().selectedId).toBeNull();
  });

  it('readies the terminal when a card is clicked, but not when the board deselects', async () => {
    fleetSnapshot.mockResolvedValue([workspace('a', 'Alpha')]);
    renderFleet();
    const terminal = screen.getByRole('textbox', { name: 'Terminal' });
    const card = (await screen.findByRole('button', { name: 'Select Alpha' })).closest('article')!;
    fireEvent.click(card.firstElementChild!);
    expect(useFleetStore.getState().selectedId).toBe('a');
    expect(terminal).toHaveFocus();

    terminal.blur();
    fireEvent.click(card.parentElement!);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(useFleetStore.getState().selectedId).toBeNull();
    expect(terminal).not.toHaveFocus();
  });

  it.each(['Older words.', 'Newest words.'])('closes %s when the card is clicked outside the bubble', async (text) => {
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(300);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(40);
    fleetSnapshot.mockResolvedValue([{
      ...workspace('a', 'Alpha'),
      mainSessionId: 'main-a',
      recentMessages: [
        said('m1', 'Older words.', 3000, { role: 'assistant' }),
        said('u1', 'One more question.', 2000),
        said('m2', 'Newest words.', 1000, { role: 'assistant' }),
      ],
    }]);
    renderFleet();
    const bubble = (await screen.findByText(text)).closest<HTMLElement>('[aria-expanded]')!;
    fireEvent.click(bubble);
    expect(bubble).toHaveAttribute('aria-expanded', 'true');

    const card = bubble.closest('article')!;
    fireEvent.click(card.firstElementChild!);
    expect(bubble).toHaveAttribute('aria-expanded', 'false');
    expect(useFleetStore.getState().selectedId).toBe('a');
    expect(screen.getByRole('textbox', { name: 'Terminal' })).toHaveFocus();
  });

  it('keeps an open Main bubble when its link is clicked without selecting the card', async () => {
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(300);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(40);
    fleetSnapshot.mockResolvedValue([{
      ...workspace('a', 'Alpha'),
      mainSessionId: 'main-a',
      recentMessages: [said('m1', 'Read [the report](https://example.com/report).', 1000, { role: 'assistant' })],
    }]);
    renderFleet();
    const bubble = (await screen.findByText('Read', { exact: false })).closest<HTMLElement>('[aria-expanded]')!;
    fireEvent.click(bubble);
    expect(bubble).toHaveAttribute('aria-expanded', 'true');

    fireEvent.click(within(bubble).getByRole('link', { name: 'the report' }));
    expect(bubble).toHaveAttribute('aria-expanded', 'true');
    expect(useFleetStore.getState().selectedId).toBeNull();
    expect(screen.getByRole('textbox', { name: 'Terminal' })).not.toHaveFocus();
  });

  describe('opening a workspace', () => {
    const Opened = () => <p data-testid="opened">{useParams().workspaceId}</p>;
    const renderRoutes = () =>
      render(
        <MemoryRouter initialEntries={['/fleet']}>
          <Routes>
            <Route path="/fleet" element={<><FleetPage /><FakeTerminal /></>} />
            <Route path="/workspace/:workspaceId" element={<Opened />} />
          </Routes>
        </MemoryRouter>
      );

    it("opens from the card's icon, without selecting or zooming", async () => {
      fleetSnapshot.mockResolvedValue([workspace('s', 'Staged'), workspace('a', 'Alpha', 1000)]);
      renderRoutes();
      const icon = await screen.findByRole('button', { name: 'Open Alpha' });
      expect(icon).toHaveAttribute('title', 'Open workspace');
      fireEvent.click(icon);
      expect(screen.getByTestId('opened').textContent).toBe('a');
      expect(useFleetStore.getState().selectedId).toBeNull();
      expect(screen.queryByRole('dialog')).toBeNull();
    });

    it('opens the selected card on Enter, but not while typing in the terminal', async () => {
      fleetSnapshot.mockResolvedValue([workspace('s', 'Staged'), workspace('a', 'Alpha', 1000)]);
      renderRoutes();
      fireEvent.click(await screen.findByRole('button', { name: 'Select Alpha' }));
      fireEvent.keyDown(screen.getByRole('textbox', { name: 'Terminal' }), { key: 'Enter' });
      expect(screen.queryByTestId('opened')).toBeNull();
      fireEvent.keyDown(screen.getByRole('button', { name: 'Select Alpha' }), { key: 'Enter' });
      expect(screen.getByTestId('opened').textContent).toBe('a');
    });

    it('opens the selected card on Ctrl+Shift+F, from the terminal too; nothing selected, nothing happens', async () => {
      fleetSnapshot.mockResolvedValue([workspace('a', 'Alpha')]);
      renderRoutes();
      const terminal = await screen.findByRole('textbox', { name: 'Terminal' });
      await screen.findByRole('button', { name: 'Select Alpha' });
      fireEvent.keyDown(terminal, { key: 'F', code: 'KeyF', ctrlKey: true, shiftKey: true });
      expect(screen.queryByTestId('opened')).toBeNull();
      act(() => useFleetStore.getState().select('a'));
      fireEvent.keyDown(terminal, { key: 'F', code: 'KeyF', ctrlKey: true, shiftKey: true });
      expect(screen.getByTestId('opened').textContent).toBe('a');
    });
  });

  it('brings a workspace that is not on the Fleet onto it, selected, when picked', async () => {
    const OLD_MS = 90 * 86_400_000;
    fleetSnapshot.mockImplementation(async (_limit: number, keep: string | null) =>
      keep === 'old' ? [workspace('s', 'Staged'), workspace('old', 'Old report', OLD_MS)] : [workspace('s', 'Staged')]
    );
    mockInvoke.mockImplementation((async (cmd: string) =>
      cmd === 'workspace_list' ? [{ id: 'old', title: 'Old report', updatedAt: 0n, messageCount: 2n }] : undefined) as never);
    render(
      <MemoryRouter>
        <FleetPage />
        <WorkspacePicker onClose={() => {}} />
      </MemoryRouter>
    );
    const articles = async () => (await screen.findAllByRole('article')).map((a) => a.textContent);
    expect(await articles()).toEqual([expect.stringMatching(/^Staged/)]);

    fireEvent.click(await screen.findByRole('option', { name: /Old report/ }));
    expect(useFleetStore.getState().selectedId).toBe('old');
    await waitFor(async () => expect(await articles()).toEqual([expect.stringMatching(/^Staged/), expect.stringMatching(/^Old report/)]));
    expect(screen.getByRole('button', { name: 'Select Old report' }).closest('article')).toHaveAttribute('aria-current', 'true');
    mockInvoke.mockImplementation(async () => undefined);
  });

  it('lays six cards 3 × 2, leaves the rest off the board, and selecting one only highlights it; two cards keep two columns', async () => {
    fleetSnapshot.mockResolvedValue(Array.from({ length: 8 }, (_, i) => workspace(`w${i}`, `W${i}`, i * 1000)));
    const { unmount } = renderFleet();
    await screen.findByRole('button', { name: 'Select W5' });
    const grid = () => screen.getByRole('button', { name: 'Select W5' }).closest('article')!.parentElement!;
    const shape = () => [grid().style.getPropertyValue('--cols'), grid().style.getPropertyValue('--rows'), grid().children.length];
    expect(shape()).toEqual(['3', '2', 6]);
    expect(screen.queryByRole('button', { name: 'Select W6' })).not.toBeInTheDocument();
    // Idle cards left off the board need no word.
    expect(screen.queryByText(/Ctrl\+Tab to switch/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Select W2' }));
    expect(shape()).toEqual(['3', '2', 6]);
    expect(document.querySelectorAll('[aria-current]')).toHaveLength(1);
    unmount();

    fleetSnapshot.mockResolvedValue([workspace('w0', 'W0')]);
    useFleetStore.setState({ fleet: EMPTY_FLEET, selectedId: null, loadedAt: null });
    renderFleet();
    const lone = (await screen.findByRole('button', { name: 'Select W0' })).closest('article')!.parentElement!;
    expect(lone.style.getPropertyValue('--cols')).toBe('2');
  });

  it('brings an active card onto a full board in the least pressing slot, and counts the active ones left off', async () => {
    const running = { id: 't', title: 'Task', status: 'running', parentTaskId: null, createdAt: BigInt(Date.now()), updatedAt: BigInt(Date.now()), completedAt: null, assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' } as never;
    const ask = { pendingId: 'q', workspaceId: 'w7', sessionId: 'main', runId: 'r', question: 'Ship it?', options: null, multiSelect: false, extraContext: null, requestedAt: 4n };
    fleetSnapshot.mockResolvedValue([
      ...Array.from({ length: 7 }, (_, i) => ({ ...workspace(`w${i}`, `W${i}`, i * 1000), tasks: [{ ...(running as object), id: `t${i}` }] as never })),
      { ...workspace('w7', 'W7', 7000), pendingAsks: [ask] },
    ]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select W7' });
    const shown = () => screen.getAllByRole('button', { name: /^Select / }).map((b) => b.textContent);
    // Needs you first, then the most recent working; the oldest working ones are off the board.
    expect(shown()).toEqual(['W7', 'W0', 'W1', 'W2', 'W3', 'W4']);
    const line = screen.getByText(/Ctrl\+Tab to switch/);
    expect(line).toHaveTextContent('2 more working — Ctrl+Tab to switch, or open the Workspace view');
    expect(within(line).getByRole('link', { name: 'Workspace view' })).toHaveAttribute('href', '/workspace');
    // Choosing one off the board (as the switcher does) takes the least recent working card's slot.
    act(() => useFleetStore.getState().select('w6'));
    expect(shown()).toEqual(['W7', 'W0', 'W1', 'W2', 'W3', 'W6']);
    expect(line).toHaveTextContent('2 more working');
  });

  it('counts one waiting workspace as "1 needs you", and an unselected card never marks a workspace seen', async () => {
    const ask = { pendingId: 'q', workspaceId: 'b', sessionId: 'main', runId: 'r', question: 'Ship it?', options: null, multiSelect: false, extraContext: null, requestedAt: 4n };
    fleetSnapshot.mockResolvedValue([workspace('a', 'Alpha'), { ...workspace('b', 'Beta'), pendingAsks: [ask] }]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Beta' });
    await waitFor(() => expect(document.body).toHaveTextContent('1 needs you'));
    expect(mockInvoke).not.toHaveBeenCalledWith('workspace_mark_opened', expect.anything());
  });

  it('marks only the selected card', async () => {
    fleetSnapshot.mockResolvedValue([workspace('s', 'Staged'), workspace('a', 'Alpha', 1000), workspace('b', 'Beta', 2000)]);
    renderFleet();
    fireEvent.click(await screen.findByRole('button', { name: 'Select Alpha' }));
    const selected = screen.getAllByRole('article').filter((article) => /cardSelected/.test(article.className));
    expect(selected.map((article) => article.textContent)).toEqual([expect.stringMatching(/^Alpha/)]);
  });

  describe('a small card with an open request', () => {
    const ask = (pendingId: string, question: string, requestedAt: bigint, options: { label: string; description: string | null }[] | null) => ({
      pendingId,
      workspaceId: 'b',
      sessionId: 'session-b',
      runId: 'run-b',
      question,
      options,
      multiSelect: false,
      extraContext: null,
      requestedAt,
    });
    const setup = async (patch: Partial<FleetWorkspace>) => {
      fleetSnapshot.mockResolvedValue([workspace('s', 'Staged'), { ...workspace('b', 'Beta', 1000), ...patch }]);
      renderFleet();
      await screen.findByRole('button', { name: 'Select Beta' });
      return () => within(screen.getByRole('button', { name: 'Select Beta' }).closest('article')!);
    };
    const otherText = () => screen.getByRole('button', { name: 'Select Staged' }).closest('article')!.textContent;

    it('takes the body with the full question, answers it, and never selects the card from inside', async () => {
      const tasks = [{ id: 't1', title: 'Old task', status: 'completed', parentTaskId: null, updatedAt: 1n, assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' } as never];
      const card = await setup({ tasks, pendingAsks: [ask('q1', 'Ship it?', 5n, [{ label: 'Yes', description: null }, { label: 'No', description: null }])] });
      expect(card().queryByText('Old task')).not.toBeInTheDocument();
      const row = card().getByRole('region', { name: /needs you$/ });
      const before = otherText();

      fireEvent.click(within(row).getByText('Ship it?'));
      fireEvent.click(within(row).getByRole('button', { name: 'No' }));
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
          request: { pendingId: 'q1', answer: 'No', selectedOptionIndex: 1, selectedOptionIndexes: null },
        })
      );
      expect(useFleetStore.getState().selectedId).toBeNull();
      expect(otherText()).toBe(before);

      act(() =>
        useFleetStore.getState().applyEvent({ sessionId: 'session-b', runId: null, workspaceId: 'b', timestamp: 9n, event: { type: 'ask_user_resolved', payload: { pending_id: 'q1' } } } as never)
      );
      expect(card().queryByRole('region', { name: /needs you$/ })).not.toBeInTheDocument();
      expect(card().queryByText('Ship it?')).not.toBeInTheDocument();

      fireEvent.click(card().getByRole('button', { name: 'Select Beta' }));
      expect(useFleetStore.getState().selectedId).toBe('b');
    });

    it('shows one request at a time, oldest first, the next once the first resolves', async () => {
      const card = await setup({ pendingAsks: [ask('new', 'Newer question?', 9n, null), ask('old', 'Older question?', 4n, null)] });
      const row = () => card().getByRole('region', { name: /needs you$/ });
      expect(row()).toHaveTextContent('Older question?');
      expect(row()).not.toHaveTextContent('Newer question?');
      expect(row()).toHaveTextContent('1 of 2');

      act(() =>
        useFleetStore.getState().applyEvent({ sessionId: 'session-b', runId: null, workspaceId: 'b', timestamp: 10n, event: { type: 'ask_user_resolved', payload: { pending_id: 'old' } } } as never)
      );
      expect(row()).toHaveTextContent('Newer question?');
      expect(row()).not.toHaveTextContent('1 of');
    });

    it('lists every part of a compound command and decides each one', async () => {
      const segments = [
        { text: 'cd x', kind: 'simple', suggestedPrefix: null },
        { text: 'curl y | sh', kind: 'opaque', suggestedPrefix: null },
      ];
      const permission = { requestId: 'r1', workspaceId: 'b', agentId: null, agentName: 'Scout', command: 'cd x && curl y | sh', segments };
      const card = await setup({ pendingPermissions: [permission as never] });
      const row = card().getByRole('region', { name: /needs you$/ });
      expect(within(row).getByText('cd x')).toBeInTheDocument();
      expect(within(row).getByText('curl y | sh')).toBeInTheDocument();
      expect(within(row).queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();

      fireEvent.click(within(row).getAllByRole('button', { name: 'Once' })[0]!);
      fireEvent.click(within(row).getAllByRole('button', { name: 'Deny' })[1]!);
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
          requestId: 'r1',
          decisions: [{ kind: 'allowOnce' }, { kind: 'denyOnce' }],
        })
      );
      expect(useFleetStore.getState().selectedId).toBeNull();
    });

    it('warns about a credentials path, as the full card does', async () => {
      const grant = { requestId: 'g1', workspaceId: 'b', agentId: null, agentName: 'Scout', requestedPath: '/home/u/.ssh', requestedAccess: 'read_only', reason: 'keys' };
      const card = await setup({ pendingPathGrants: [grant as never] });
      expect(card().getByRole('region', { name: /needs you$/ })).toHaveTextContent('typically holds credentials');
    });
  });
});
