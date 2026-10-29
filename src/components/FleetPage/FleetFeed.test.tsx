import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import type { AssistantEventEnvelope, AssistantMessage, AssistantUiEvent, FleetWorkspace, WorkspaceTaskResponse } from '../../generated/bindings';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => undefined) }));

/** Every markdown text parsed, in order. */
const parsed = vi.hoisted(() => [] as string[]);
vi.mock('react-markdown', async (importOriginal) => {
  const actual = await importOriginal<typeof import('react-markdown')>();
  const { createElement } = await import('react');
  const Counted = (props: Parameters<typeof actual.default>[0]) => {
    parsed.push(String(props.children));
    return createElement(actual.default, props);
  };
  return { ...actual, default: Counted };
});

import { invoke } from '@tauri-apps/api/core';
import FleetFeed from './FleetFeed';
import { castFor } from './FleetFace';
import { EMPTY_FLEET, applyAssistantEvent, applyUserSent, seedFleet, type FleetLive, type FleetState } from '../../fleet/fleetModel';
import { cardFeed } from '../../fleet/fleetFeed';

const WS = 'ws-1';
const MAIN = 'main-session';
const NOW = Date.UTC(2026, 9, 18, 12);
const IDLE: FleetLive = { runs: {}, tasks: {} };
const RUNNING: FleetLive = { runs: { [WS]: ['r1'] }, tasks: {} };

const message = (id: string, content: AssistantMessage['content']): AssistantMessage =>
  ({ id, sessionId: MAIN, role: 'assistant', content, createdAt: 0n, providerMetadata: null, origin: { kind: 'user' } }) as AssistantMessage;
const text = (t: string) => ({ type: 'text', text: t }) as AssistantMessage['content'][number];
const tool = (id: string) => ({ type: 'tool_use', tool_call_id: id, tool_name: 'bash_exec', arguments: {} }) as AssistantMessage['content'][number];
const event = (event: AssistantUiEvent, at: number): AssistantEventEnvelope =>
  ({ sessionId: MAIN, runId: 'r1', workspaceId: WS, timestamp: BigInt(at), event }) as AssistantEventEnvelope;
const updated = (m: AssistantMessage, at: number) => event({ type: 'assistant_message_updated', payload: { message: m } } as AssistantUiEvent, at);

/** Main, in run `r1`, writes a block of text, calls a tool, and writes another. */
const fleet = (...events: AssistantEventEnvelope[]): FleetState =>
  events.reduce(applyAssistantEvent, applyUserSent(EMPTY_FLEET, WS, MAIN, null, 0));
const twoBlocks = (second = 'After that, the fix.') =>
  fleet(updated(message('m1', [text('First, the leaks.'), tool('c1')]), NOW - 2000), updated(message('m1', [text('First, the leaks.'), tool('c1'), text(second), tool('c2')]), NOW - 1000));

const Feed = ({ state, live = IDLE, now = NOW, onOpen = () => {} }: { state: FleetState; live?: FleetLive; now?: number; onOpen?: (id: string) => void }) => (
  <FleetFeed workspaceId={WS} feed={cardFeed(state.cards[WS]!, live, now)} shouts={new Set()} now={now} faces={castFor({}, null)} onOpen={onOpen} />
);
const saying = (markdown: string) => fleet(updated(message('m1', [text(markdown), tool('c1')]), NOW - 1000));

const bubbleOf = (words: RegExp) => screen.getByText(words).closest<HTMLElement>('[role=button]')!;
const mainBubble = () => bubbleOf(/First, the leaks/);
const words = () => mainBubble().querySelector<HTMLElement>('[data-bubble] > div')!;

describe("Main's bubble", () => {
  it('shows its newest lines while it is the last bubble, run done or not, and its first once another follows', () => {
    const height = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(1000);
    try {
      const { rerender } = render(<Feed state={twoBlocks()} live={RUNNING} />);
      expect(words()).toHaveAttribute('data-newest');
      expect(words()).toHaveTextContent('First, the leaks.');
      expect(words()).toHaveTextContent('After that, the fix.');
      rerender(<Feed state={twoBlocks()} live={IDLE} />);
      expect(words()).toHaveAttribute('data-newest');

      const asked = applyUserSent(twoBlocks(), WS, MAIN, { ...message('u1', [text('And the docs?')]), role: 'user' }, NOW);
      rerender(<Feed state={asked} now={NOW + 2000} />);
      expect(words()).not.toHaveAttribute('data-newest');

      const answered = applyAssistantEvent(asked, updated(message('m2', [text('Docs next.'), tool('c3')]), NOW + 1000));
      rerender(<Feed state={answered} now={NOW + 2000} />);
      const reply = bubbleOf(/Docs next/).querySelector('[data-bubble] > div');
      expect(reply).toHaveAttribute('data-newest');
      expect(words()).not.toHaveAttribute('data-newest');
    } finally {
      height.mockRestore();
    }
  });

  it('opens in place on a click, and keeps growing open; it never zooms', () => {
    const { rerender } = render(<Feed state={twoBlocks()} live={RUNNING} />);
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'false');
    fireEvent.click(mainBubble());
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'true');
    // Open, it scrolls inside instead of following the newest lines.
    expect(words()).toHaveAttribute('data-open');
    expect(words()).not.toHaveAttribute('data-newest');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    rerender(<Feed state={twoBlocks('After that, the fix. Then the docs.')} live={RUNNING} />);
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'true');
    expect(words()).toHaveTextContent('Then the docs.');
  });

  it('leaves clicks in it open to the reader; a press outside or Esc closes it', () => {
    render(<Feed state={twoBlocks()} />);
    const open = () => mainBubble().getAttribute('aria-expanded');
    fireEvent.keyDown(mainBubble(), { key: 'Enter' });
    expect(open()).toBe('true');
    // A click in it: its words are the reader's to select.
    fireEvent.click(words());
    expect(open()).toBe('true');
    // A selection dragged out of it starts inside and ends in a click on what holds it both.
    const scene = mainBubble().closest<HTMLElement>('[data-mood]')!;
    fireEvent.pointerDown(words());
    fireEvent.click(scene);
    expect(open()).toBe('true');

    // Esc closes it, and is its alone: the page keeps the card selected.
    expect(fireEvent.keyDown(words(), { key: 'Escape' })).toBe(false);
    expect(open()).toBe('false');

    fireEvent.click(mainBubble());
    expect(open()).toBe('true');
    fireEvent.pointerDown(scene);
    expect(open()).toBe('false');
    expect(words()).not.toHaveAttribute('data-open');

    // Enter or Space opens and closes it, as a disclosure.
    fireEvent.keyDown(mainBubble(), { key: ' ' });
    expect(open()).toBe('true');
    fireEvent.keyDown(mainBubble(), { key: 'Enter' });
    expect(open()).toBe('false');
  });

  it('fetches a reply the snapshot cut whole, once while it is open, not again on each recompute', async () => {
    const preview = { message: { ...message('m1', [text('First, the')]), createdAt: BigInt(NOW - 1000) }, runId: 'r1', truncated: true };
    const snapshot = { workspaceId: WS, mainSessionId: MAIN, lastActivityAt: 0n, lastOpenedAt: 0n, lastMainRun: null, lastAssistantMessage: null, recentMessages: [preview],
      pendingAsks: [], pendingPermissions: [], pendingPathGrants: [], agents: [] } as unknown as FleetWorkspace;
    const cut = () => cardFeed(seedFleet(EMPTY_FLEET, [snapshot]).cards[WS]!, IDLE, NOW);
    const fetched = vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === 'assistant_get_message' ? message('m1', [text('First, the leaks.')]) : undefined));
    const fetches = () => fetched.mock.calls.filter(([cmd]) => cmd === 'assistant_get_message').length;
    const at = fetches();
    try {
      const { rerender } = render(<FleetFeed workspaceId={WS} feed={cut()} shouts={new Set()} now={NOW} faces={castFor({}, null)} onOpen={() => {}} />);
      fireEvent.click(bubbleOf(/First, the…/));
      expect(await screen.findByText('First, the leaks.')).toBeInTheDocument();
      // Every Fleet event recomputes the feed: new lines, the same message.
      rerender(<FleetFeed workspaceId={WS} feed={cut()} shouts={new Set()} now={NOW + 1} faces={castFor({}, null)} onOpen={() => {}} />);
      expect(mainBubble()).toHaveAttribute('aria-expanded', 'true');
      expect(fetches() - at).toBe(1);
    } finally {
      fetched.mockImplementation(async () => undefined);
    }
  });

  it('keeps an older Main bubble expanded and gives it the card space', () => {
    const height = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(200);
    const rowHeight = vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
      if (this.tagName === 'P') return 20;
      return this.getAttribute('aria-expanded') === 'true' ? 120 : 40;
    });
    try {
      const first = applyUserSent(EMPTY_FLEET, WS, MAIN, { ...message('u0', [text('Earlier.')]), role: 'user' }, NOW - 3000);
      const main = applyAssistantEvent(first, updated(message('m1', [text('First, the leaks.')]), NOW - 2000));
      const next = applyUserSent(main, WS, MAIN, { ...message('u1', [text('Next reply.')]), role: 'user' }, NOW - 1000);
      const state = applyUserSent(next, WS, MAIN, { ...message('u2', [text('Newest reply.')]), role: 'user' }, NOW);
      render(<Feed state={state} />);

      fireEvent.click(mainBubble());
      expect(screen.getByText('First, the leaks.').closest('[aria-expanded]')).toHaveAttribute('aria-expanded', 'true');
      expect(mainBubble().closest('[data-mood]')).toHaveAttribute('data-expanded', 'true');
      fireEvent.keyDown(mainBubble(), { key: 'Escape' });
      expect(mainBubble()).toHaveAttribute('aria-expanded', 'false');
      expect(mainBubble().closest('[data-mood]')).not.toHaveAttribute('data-expanded');
      expect(screen.getByRole('list', { name: 'Earlier' })).toBeInTheDocument();
    } finally {
      height.mockRestore();
      rowHeight.mockRestore();
    }
  });

  it('releases the feed when the expanded message leaves it', () => {
    const { rerender } = render(<Feed state={saying('First, the leaks.')} />);
    fireEvent.click(mainBubble());
    expect(mainBubble().closest('[data-mood]')).toHaveAttribute('data-expanded', 'true');

    rerender(<Feed state={fleet(updated(message('m2', [text('Other words.'), tool('c2')]), NOW))} />);
    expect(screen.getByText('Other words.')).toBeInTheDocument();
    expect(screen.getByText('Other words.').closest('[data-mood]')).not.toHaveAttribute('data-expanded');
  });

  it('says its words as markdown, closed and open', () => {
    render(<Feed state={saying('First, the leaks, in **two** places:\n\n- the pool\n- the cache')} />);
    const shown = () => {
      expect(within(words()).getByText('two').tagName).toBe('STRONG');
      expect(within(words()).getAllByRole('listitem').map((li) => li.textContent)).toEqual(['the pool', 'the cache']);
    };
    shown();
    fireEvent.click(mainBubble());
    expect(words()).toHaveAttribute('data-open');
    shown();
  });

  it('keeps tool-separated text in distinct paragraphs in a bubble and its zoom', () => {
    const state = fleet(updated(message('m1', [text('Before.'), tool('c1'), text('After.')]), NOW - 1000));
    const { rerender } = render(<Feed state={state} />);
    expect([...bubbleOf(/Before/).querySelectorAll('p')].map((p) => p.textContent)).toEqual(['Before.', 'After.']);

    const later = applyUserSent(
      applyUserSent(state, WS, MAIN, { ...message('u1', [text('First reply.')]), role: 'user' }, NOW),
      WS, MAIN, { ...message('u2', [text('Second reply.')]), role: 'user' }, NOW + 1
    );
    const afterThirdReply = applyUserSent(later, WS, MAIN, { ...message('u3', [text('Third reply.')]), role: 'user' }, NOW + 2);
    const height = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(200);
    const rowHeight = vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.tagName === 'P' ? 20 : 40;
    });
    try {
      rerender(<Feed state={afterThirdReply} />);
      const row = within(screen.getByRole('list', { name: 'Earlier' })).getByText(/Before/).closest('button')!;
      fireEvent.click(row);
      expect([...screen.getByRole('dialog').querySelectorAll('p')].map((p) => p.textContent).filter((t) => t === 'Before.' || t === 'After.')).toEqual(['Before.', 'After.']);
    } finally {
      height.mockRestore();
      rowHeight.mockRestore();
    }
  });

  it('shows a code block as its source, a diagram or chart spec too, never drawn', () => {
    render(<Feed state={saying('First, the leaks:\n\n```mermaid\ngraph TD; A-->B\n```\n\n```vega-lite\n{"mark": "bar"}\n```')} />);
    expect([...words().querySelectorAll('pre > code')].map((c) => c.textContent)).toEqual(['graph TD; A-->B', '{"mark": "bar"}']);
    expect(mainBubble().querySelector('canvas, svg[role=graphics-document], [class*=mermaid]')).toBeNull();
  });

  it('parses its markdown again only when its words change', () => {
    parsed.length = 0;
    const { rerender } = render(<Feed state={twoBlocks()} live={RUNNING} />);
    const times = () => parsed.filter((t) => t.startsWith('First, the leaks.')).length;
    expect(times()).toBe(1);
    // The clock, a new word of yours, a new callback: the same words.
    rerender(<Feed state={applyUserSent(twoBlocks(), WS, MAIN, { ...message('u2', [text('Thanks')]), role: 'user' }, NOW)} live={RUNNING} now={NOW + 60_000} onOpen={() => {}} />);
    expect(screen.getByText(/Thanks/)).toBeInTheDocument();
    expect(times()).toBe(1);
    rerender(<Feed state={twoBlocks('After that, the fix. Then the docs.')} live={RUNNING} />);
    expect(times()).toBe(2);
  });

  it('draws a chart as a chip that opens the workspace, without opening the bubble', () => {
    const onOpen = vi.fn();
    render(<Feed state={saying('First, the leaks:\n\n- by day: ![Leaks by day](charts/leaks.vl.json), worst on Monday\n\n![The pool](shots/pool.png)')} onOpen={onOpen} />);
    expect(within(mainBubble()).getByRole('button', { name: /The pool/ })).toBeInTheDocument();
    const chip = within(mainBubble()).getByRole('button', { name: /Leaks by day/ });
    // Through the markdown: the chip sits in its list item.
    expect(chip.closest('li')).toHaveTextContent('by day: Leaks by day, worst on Monday');
    expect(mainBubble().querySelector('img, canvas, svg[role=graphics-document]')).toBeNull();
    fireEvent.click(chip);
    expect(onOpen).toHaveBeenCalledWith(WS);
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'false');
  });

  it('leaves Enter and Space on a chip or link in it to that chip or link', () => {
    render(<Feed state={saying('First, the leaks: ![Leaks by day](charts/leaks.vl.json), see [the issue](https://example.com/1)')} />);
    const chip = within(mainBubble()).getByRole('button', { name: /Leaks by day/ });
    const link = within(mainBubble()).getByRole('link', { name: 'the issue' });
    for (const inner of [chip, link]) {
      // Not prevented: the chip or link activates itself.
      expect(fireEvent.keyDown(inner, { key: 'Enter' })).toBe(true);
      expect(fireEvent.keyDown(inner, { key: ' ' })).toBe(true);
    }
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'false');
  });
});

/**
 * Main's newest bubble as a 100px scroller over `tall()` px of words (the card
 * roomy around it), sliding text in with `glide` unless motion is reduced.
 */
const scrolling = (tall: () => number, { reduce = false } = {}) => {
  const glide = vi.fn();
  Object.defineProperty(HTMLElement.prototype, 'animate', { value: glide, configurable: true });
  vi.stubGlobal('matchMedia', (query: string) => ({ matches: reduce && query.includes('reduce') }));
  const spies = [
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.matches('[data-newest], [data-open]') ? 100 : 1000;
    }),
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.parentElement?.matches('[data-newest], [data-open]') ? tall() : 0;
    }),
    // A slide's transform stretches what scrolls past the words' own height.
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.matches('[data-newest], [data-open]') ? tall() + (glide.mock.calls.length > 0 ? 60 : 0) : 0;
    }),
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(200),
  ];
  return {
    glide,
    restore: () => {
      spies.forEach((spy) => spy.mockRestore());
      delete (HTMLElement.prototype as Partial<HTMLElement>).animate;
      vi.unstubAllGlobals();
    },
  };
};

describe("Main's newest bubble, scrolling", () => {
  const writing = (...more: string[]) => <Feed state={twoBlocks(['After that, the fix.', ...more].join(' '))} live={RUNNING} />;

  it('follows its end, sliding new lines in; stays where the reader scrolled up; follows again once back at the end', () => {
    let tall = 200;
    const { glide, restore } = scrolling(() => tall);
    try {
      const { rerender } = render(writing());
      expect(words().scrollTop).toBe(100);
      expect(glide).not.toHaveBeenCalled();

      tall = 260;
      rerender(writing('Then the docs.'));
      expect(words().scrollTop).toBe(160);
      expect(glide).toHaveBeenCalledTimes(1);
      expect(glide.mock.calls[0]![0]).toEqual([{ transform: 'translateY(60px)' }, { transform: 'none' }]);
      expect(glide.mock.calls[0]![1]).toMatchObject({ composite: 'add' });

      words().scrollTop = 50;
      tall = 320;
      rerender(writing('Then the docs.', 'Then the tests.'));
      expect(words().scrollTop).toBe(50);
      expect(glide).toHaveBeenCalledTimes(1);

      words().scrollTop = 219.5;
      tall = 530;
      rerender(writing('Then the docs.', 'Then the tests.', 'Done.'));
      expect(words().scrollTop).toBe(430);
      expect(glide).toHaveBeenCalledTimes(2);
      // A slide starts at most a bubble's height below.
      expect(glide.mock.calls[1]![0][0]).toEqual({ transform: 'translateY(100px)' });
    } finally {
      restore();
    }
  });

  it('follows without sliding under reduced motion', () => {
    let tall = 200;
    const { glide, restore } = scrolling(() => tall, { reduce: true });
    try {
      const { rerender } = render(writing());
      tall = 260;
      rerender(writing('Then the docs.'));
      expect(words().scrollTop).toBe(160);
      expect(glide).not.toHaveBeenCalled();
    } finally {
      restore();
    }
  });

  it('shows its start once another bubble follows or while open, and its end again once closed', () => {
    const { restore } = scrolling(() => 200);
    try {
      const { rerender } = render(writing());
      expect(words().scrollTop).toBe(100);
      fireEvent.click(mainBubble());
      expect(words().scrollTop).toBe(0);
      fireEvent.keyDown(mainBubble(), { key: 'Escape' });
      expect(words().scrollTop).toBe(100);

      rerender(<Feed state={applyUserSent(twoBlocks(), WS, MAIN, { ...message('u1', [text('And the docs?')]), role: 'user' }, NOW)} now={NOW + 2000} />);
      expect(words()).not.toHaveAttribute('data-newest');
      expect(words().scrollTop).toBe(0);
    } finally {
      restore();
    }
  });

  it('keeps its end in view through a new size, unless the reader scrolled up', () => {
    let tall = 200;
    const { restore } = scrolling(() => tall);
    const observers: { targets: Element[]; resized: () => void }[] = [];
    vi.stubGlobal(
      'ResizeObserver',
      class {
        targets: Element[] = [];
        constructor(public resized: () => void) {
          observers.push(this);
        }
        observe(target: Element) {
          this.targets.push(target);
        }
        disconnect() {}
      }
    );
    const resize = (newTall: number) => {
      tall = newTall;
      observers.filter((o) => o.targets.includes(words())).forEach((o) => o.resized());
    };
    try {
      const { rerender } = render(writing());
      expect(words().scrollTop).toBe(100);
      resize(260);
      expect(words().scrollTop).toBe(160);
      tall = 300;
      rerender(writing('Then the docs.'));
      expect(words().scrollTop).toBe(200);
      // Mid-slide: the same size, the scroll range stretched by the slide.
      resize(300);
      expect(words().scrollTop).toBe(200);
      tall = 340;
      rerender(writing('Then the docs.', 'Then the tests.'));
      expect(words().scrollTop).toBe(240);
      words().scrollTop = 40;
      resize(400);
      expect(words().scrollTop).toBe(40);
    } finally {
      restore();
    }
  });

  it('leaves a click on its scrollbar to the scrollbar', () => {
    const { restore } = scrolling(() => 200);
    try {
      render(writing());
      fireEvent.click(words(), { clientX: 203 });
      expect(mainBubble()).toHaveAttribute('aria-expanded', 'false');
      fireEvent.click(words(), { clientX: 120 });
      expect(mainBubble()).toHaveAttribute('aria-expanded', 'true');
    } finally {
      restore();
    }
  });
});

describe('a task in the feed', () => {
  const queued = { id: 't1', workspaceId: WS, title: 'Draft the release notes', status: 'queued', sessionId: 'task-session', parentTaskId: null, cycleNo: 1n,
    assignedAgentDisplayName: 'Writer', assignedAgentDefinitionId: 'def-w', assignedToWorkspaceAgentId: 'wa-w', createdAt: BigInt(NOW - 1000), updatedAt: BigInt(NOW - 1000) } as unknown as WorkspaceTaskResponse;

  it('says a queued task is next', () => {
    render(<Feed state={saying('First, the leaks.')} live={{ runs: {}, tasks: { [WS]: { t1: queued } } }} />);
    expect(bubbleOf(/Draft the release notes/)).toHaveTextContent(/· next ·/);
  });
});

describe('pictures in the feed', () => {
  const picture = (id: string) =>
    ({ type: 'image', id, path: `.clai/images/${id}.png`, media_type: 'image/png', filename: `${id}.png` }) as AssistantMessage['content'][number];
  const yours = (id: string, content: AssistantMessage['content'], at = NOW) =>
    ({ ...message(id, content), role: 'user', createdAt: BigInt(at) }) as AssistantMessage;
  const created = (m: AssistantMessage) => event({ type: 'message_created', payload: { message: m } } as AssistantUiEvent, Number(m.createdAt));
  const seeded = (...messages: AssistantMessage[]) =>
    seedFleet(EMPTY_FLEET, [
      { workspaceId: WS, mainSessionId: MAIN, lastActivityAt: 0n, lastOpenedAt: 0n, lastMainRun: null, lastAssistantMessage: null,
        recentMessages: messages.map((m) => ({ message: m, runId: null, truncated: false })),
        pendingAsks: [], pendingPermissions: [], pendingPathGrants: [], agents: [] } as unknown as FleetWorkspace,
    ]);

  beforeEach(() => {
    vi.mocked(invoke).mockImplementation(async (cmd) =>
      cmd === 'workspace_read_file_base64' ? { path: '', mime: 'image/png', base64: 'QUJD' } : undefined
    );
  });
  afterEach(() => {
    vi.mocked(invoke).mockImplementation(async () => undefined);
  });

  it('makes a bubble of a message that is only a picture, heard live or seeded, with its thumbnail', async () => {
    const shot = yours('u1', [picture('shot')]);
    for (const state of [fleet(created(shot)), seeded(shot)]) {
      const { unmount } = render(<Feed state={state} />);
      const thumb = await screen.findByAltText('shot.png');
      expect(thumb).toHaveAttribute('src', 'data:image/png;base64,QUJD');
      expect(thumb.closest('[data-bubble]')).not.toBeNull();
      unmount();
    }
  });

  it('shows three thumbnails, then how many more', async () => {
    render(<Feed state={fleet(created(yours('u1', [text('Five shots.'), ...['a', 'b', 'c', 'd', 'e'].map(picture)])))} />);
    const bubble = bubbleOf(/Five shots/);
    expect(await within(bubble).findAllByRole('img')).toHaveLength(3);
    expect(bubble).toHaveTextContent('+2');
  });

  /** Room for three bubbles, so the oldest of four, with pictures `ids`, is a row. */
  const fourSaid = (ids: string[], last = 'Last.') =>
    fleet(...[yours('u0', [text('Two shots.'), ...ids.map(picture)], NOW - 3000), yours('u1', [text('Next.')], NOW - 2000),
      yours('u2', [text('Then.')], NOW - 1000), yours('u3', [text(last)], NOW)].map(created));
  const measured = () => {
    const height = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(200);
    const rowHeight = vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.tagName === 'P' ? 20 : 40;
    });
    return () => {
      height.mockRestore();
      rowHeight.mockRestore();
    };
  };

  it('reads a row\'s pictures once while the feed re-measures around it', () => {
    const restore = measured();
    try {
      const ids = ['once-a', 'once-b'];
      const reads = () => vi.mocked(invoke).mock.calls.filter(([cmd, args]) =>
        cmd === 'workspace_read_file_base64' && ids.some((id) => JSON.stringify(args).includes(id))).length;
      const { rerender } = render(<Feed state={fourSaid(ids)} />);
      expect(within(screen.getByRole('list', { name: 'Earlier' })).getByText(/Two shots/)).toBeInTheDocument();
      for (const last of ['Last, a', 'Last, and b', 'Last, and then c']) rerender(<Feed state={fourSaid(ids, last)} />);
      expect(reads()).toBe(ids.length);
    } finally {
      restore();
    }
  });

  const readsOf = (id: string) =>
    vi.mocked(invoke).mock.calls.filter(([cmd, args]) => cmd === 'workspace_read_file_base64' && JSON.stringify(args).includes(`/${id}.png`)).length;
  const shown = async (id: string) => {
    const { unmount } = render(<Feed state={fleet(created(yours('u1', [picture(id)])))} />);
    await screen.findByAltText(`${id}.png`);
    unmount();
  };

  it('shows a picture it already read at once', async () => {
    await shown('again');
    render(<Feed state={fleet(created(yours('u1', [picture('again')])))} />);
    expect(screen.getByAltText('again.png')).toHaveAttribute('src', 'data:image/png;base64,QUJD');
  });

  it('reads a picture again after a read that failed', async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error('gone'));
    const { unmount } = render(<Feed state={fleet(created(yours('u1', [picture('retry')])))} />);
    expect(await screen.findByText('Image unavailable')).toBeInTheDocument();
    unmount();
    await shown('retry');
    expect(readsOf('retry')).toBe(2);
  });

  it('keeps the pictures shown last while they fit in 64 MiB, and reads an older one again', async () => {
    // Each is 25 million characters: two fit, a third does not.
    const big = 'A'.repeat(25_000_000);
    vi.mocked(invoke).mockImplementation(async (cmd) => (cmd === 'workspace_read_file_base64' ? { path: '', mime: 'image/png', base64: big } : undefined));
    for (const id of ['big-a', 'big-b', 'big-c', 'big-b', 'big-a']) await shown(id);
    expect([readsOf('big-a'), readsOf('big-b'), readsOf('big-c')]).toEqual([2, 1, 1]);
  });

  it('zooms a bubble from its thumbnail, and keeps the zoom open when its line becomes a row', async () => {
    const restore = measured();
    try {
      const said = [yours('u0', [text('Two shots.'), picture('fit-a')], NOW - 3000), yours('u1', [text('Next.')], NOW - 2000),
        yours('u2', [text('Then.')], NOW - 1000)].map(created);
      const { rerender } = render(<Feed state={fleet(...said)} />);
      fireEvent.click(await screen.findByAltText('fit-a.png'));
      const zoom = screen.getByRole('dialog', { name: 'Message from You' });
      expect(within(zoom).getByAltText('fit-a.png')).toHaveAttribute('src', 'data:image/png;base64,QUJD');
      rerender(<Feed state={fleet(...said, created(yours('u3', [text('Last.')], NOW)))} />);
      expect(within(screen.getByRole('list', { name: 'Earlier' })).getByText(/Two shots/)).toBeInTheDocument();
      expect(screen.getByRole('dialog')).toBe(zoom);
    } finally {
      restore();
    }
  });

  it('marks an older row that has pictures, and its zoom shows them', async () => {
    const restore = measured();
    try {
      render(<Feed state={fourSaid(['a', 'b'])} />);
      const row = within(screen.getByRole('list', { name: 'Earlier' })).getByText(/Two shots/).closest('button')!;
      expect(within(row).getByRole('img', { name: '2 images' })).toBeInTheDocument();
      expect(within(row).queryByAltText('a.png')).toBeNull();
      fireEvent.click(row);
      const zoom = screen.getByRole('dialog');
      expect(await within(zoom).findByAltText('a.png')).toHaveAttribute('src', 'data:image/png;base64,QUJD');
      expect(within(zoom).getByAltText('b.png')).toBeInTheDocument();
    } finally {
      restore();
    }
  });
});
