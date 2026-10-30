import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import type { AssistantEventEnvelope, AssistantMessage, AssistantUiEvent, FleetHighlight, FleetWorkspace, WorkspaceTaskResponse } from '../../generated/bindings';

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
import { EMPTY_FLEET, applyAssistantEvent, applyUserSent, applyWholeMessage, seedFleet, type FleetLive, type FleetState } from '../../fleet/fleetModel';
import { FEED_LIMIT, cardFeed } from '../../fleet/fleetFeed';
import { useFleetStore } from '../../fleet/fleetStore';

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
/** A snapshot of the card with these recent messages. */
const snapshotWith = (...recentMessages: FleetHighlight[]) =>
  ({ workspaceId: WS, mainSessionId: MAIN, lastActivityAt: 0n, lastOpenedAt: 0n, lastMainRun: null, lastAssistantMessage: null, recentMessages,
    pendingAsks: [], pendingPermissions: [], pendingPathGrants: [], agents: [] }) as unknown as FleetWorkspace;
const saying = (markdown: string) => fleet(updated(message('m1', [text(markdown), tool('c1')]), NOW - 1000));

const bubbleOf = (words: RegExp) => screen.getByText(words).closest<HTMLElement>('[role=button]')!;
const mainBubble = () => bubbleOf(/First, the leaks/);
const words = () => mainBubble().querySelector<HTMLElement>('[data-bubble] > div')!;

describe("Main's bubble", () => {
  it('zooms out on a click, Enter or Space, like every bubble; it never opens in place', () => {
    render(<Feed state={twoBlocks()} live={RUNNING} />);
    const bubble = mainBubble();
    expect(bubble).toHaveAttribute('aria-haspopup', 'dialog');
    fireEvent.click(bubble);
    const zoom = screen.getByRole('dialog', { name: 'Message from Main' });
    expect(zoom).toHaveTextContent('After that, the fix.');
    expect(bubble).not.toHaveAttribute('aria-expanded');
    fireEvent.keyDown(zoom, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    for (const key of ['Enter', ' ']) {
      expect(fireEvent.keyDown(bubble, { key })).toBe(false);
      fireEvent.keyDown(screen.getByRole('dialog', { name: 'Message from Main' }), { key: 'Escape' });
      expect(screen.queryByRole('dialog')).toBeNull();
    }
  });

  it('says a reply the snapshot cut whole once fetched, in its bubble and its zoom; fetched once, not on each recompute', async () => {
    const preview = { message: { ...message('m1', [text('First, the')]), createdAt: BigInt(NOW - 1000) }, runId: 'r1', truncated: true };
    useFleetStore.setState({ fleet: seedFleet(EMPTY_FLEET, [snapshotWith(preview)]), buffer: null });
    let release = () => {};
    const fetched = vi.mocked(invoke).mockImplementation((cmd) =>
      cmd === 'assistant_get_message'
        ? new Promise((resolve) => (release = () => resolve(message('m1', [text('First, the leaks.'), tool('c1'), text('Then the fix.')]))))
        : Promise.resolve(undefined)
    );
    const fetches = () => fetched.mock.calls.filter(([cmd]) => cmd === 'assistant_get_message').length;
    const at = fetches();
    /** The card as the store has it, as every Fleet event recomputes it. */
    const Live = ({ now }: { now: number }) => {
      const card = useFleetStore((s) => s.fleet.cards[WS]!);
      return <FleetFeed workspaceId={WS} feed={cardFeed(card, IDLE, now)} shouts={new Set()} now={now} faces={castFor({}, null)} onOpen={() => {}} />;
    };
    const { rerender, unmount } = render(<Live now={NOW} />);
    try {
      const shown = () => bubbleOf(/First, the/).querySelector('[data-bubble] > div')!;
      // The preview, until the whole reply arrives.
      expect(shown()).toHaveTextContent('First, the…');
      await act(async () => release());
      expect(shown()).toHaveTextContent('First, the leaks.');
      expect(shown()).toHaveTextContent('Then the fix.');
      expect(shown()).not.toHaveTextContent('…');
      expect(shown()).not.toHaveTextContent('bash_exec');
      rerender(<Live now={NOW + 1} />);
      fireEvent.click(mainBubble());
      expect(screen.getByRole('dialog')).toHaveTextContent('Then the fix.');
      expect(fetches() - at).toBe(1);
    } finally {
      unmount();
      fetched.mockImplementation(async () => undefined);
      useFleetStore.setState({ fleet: EMPTY_FLEET });
    }
  });

  describe('a bubble joining replies the snapshot cut', () => {
    /** Main's long reply `id`, as the snapshot cut it to 600 characters, and whole. */
    const long = (id: string, at: number) => {
      const whole = `${id} starts. ${'Lorem ipsum dolor sit amet. '.repeat(120)}${id} ends here.`;
      const createdAt = BigInt(at);
      return {
        length: whole.length,
        whole: { ...message(id, [text(whole)]), createdAt },
        preview: { message: { ...message(id, [text(whole.slice(0, 600))]), createdAt }, runId: 'r1', truncated: true },
      };
    };
    const first = long('m1', NOW - 2000);
    const second = long('m2', NOW - 1000);
    const Live = () => {
      const card = useFleetStore((s) => s.fleet.cards[WS]);
      return card ? <FleetFeed workspaceId={WS} feed={cardFeed(card, IDLE, NOW)} shouts={new Set()} now={NOW} faces={castFor({}, null)} onOpen={() => {}} /> : null;
    };
    /** Serves each whole reply, failing the first `failures` fetches. */
    const serve = (failures: number) => {
      let failed = 0;
      return vi.mocked(invoke).mockImplementation(async (cmd, args) => {
        if (cmd !== 'assistant_get_message') return undefined;
        if (failed++ < failures) throw new Error('offline');
        return [first.whole, second.whole].find((m) => m.id === (args as { messageId: string }).messageId);
      });
    };
    const fetchesOf = (fetched: ReturnType<typeof serve>) =>
      fetched.mock.calls.flatMap(([cmd, args]) => (cmd === 'assistant_get_message' ? [(args as { messageId: string }).messageId] : []));
    beforeEach(() => {
      vi.mocked(invoke).mockClear();
      useFleetStore.setState({ fleet: seedFleet(EMPTY_FLEET, [snapshotWith(first.preview, second.preview)]), buffer: null });
    });
    afterEach(() => {
      vi.mocked(invoke).mockImplementation(async () => undefined);
      useFleetStore.setState({ fleet: EMPTY_FLEET });
    });

    it('fetches each of them, so its zoom says both whole, past the preview', async () => {
      const fetched = serve(0);
      render(<Live />);
      await act(async () => {});
      expect(new Set(fetchesOf(fetched))).toEqual(new Set(['m1', 'm2']));
      fireEvent.click(bubbleOf(/m1 starts/));
      const zoom = screen.getByRole('dialog', { name: 'Message from Main' });
      expect(zoom).toHaveTextContent('m1 ends here.');
      expect(zoom).toHaveTextContent('m2 ends here.');
      expect(zoom).not.toHaveTextContent('…');
      expect(zoom.textContent!.length).toBeGreaterThan(first.length + second.length);
    });

    it('fetches again when zoomed after a fetch failed', async () => {
      const fetched = serve(2);
      render(<Live />);
      await act(async () => {});
      expect(bubbleOf(/m1 starts/)).toHaveTextContent('…');
      await act(async () => {
        fireEvent.click(bubbleOf(/m1 starts/));
      });
      expect(fetchesOf(fetched).sort()).toEqual(['m1', 'm1', 'm2', 'm2']);
      expect(screen.getByRole('dialog')).toHaveTextContent('m2 ends here.');
    });

    it("says them whole in a zoom whose line left the card's newest lines before they arrived", async () => {
      const pending: (() => void)[] = [];
      vi.mocked(invoke).mockImplementation((cmd, args) =>
        cmd === 'assistant_get_message'
          ? new Promise((resolve) => pending.push(() => resolve([first.whole, second.whole].find((m) => m.id === (args as { messageId: string }).messageId))))
          : Promise.resolve(undefined)
      );
      render(<Live />);
      fireEvent.click(bubbleOf(/m1 starts/));
      await act(async () => {
        for (let i = 0; i < FEED_LIMIT; i++) {
          const later = { ...message(`n${i}`, [text(`Later ${i}.`)]), createdAt: BigInt(NOW + i) };
          useFleetStore.getState().dispatch((s) => applyAssistantEvent(s, { ...event({ type: 'assistant_message_completed', payload: { message: later } } as AssistantUiEvent, NOW + i), runId: `later-${i}` }));
        }
      });
      expect(screen.queryByText(/m1 starts/, { selector: '[data-bubble] *' })).toBeNull();
      await act(async () => pending.forEach((settle) => settle()));
      expect(screen.getByRole('dialog')).toHaveTextContent('m2 ends here.');
    });
  });

  it('says its words as markdown, in its bubble and its zoom', () => {
    render(<Feed state={saying('First, the leaks, in **two** places:\n\n- the pool\n- the cache')} />);
    const shown = (where: HTMLElement) => {
      expect(within(where).getByText('two').tagName).toBe('STRONG');
      expect(within(where).getAllByRole('listitem').map((li) => li.textContent)).toEqual(['the pool', 'the cache']);
    };
    shown(words());
    fireEvent.click(mainBubble());
    shown(screen.getByRole('dialog'));
  });

  it('keeps tool-separated text in distinct paragraphs in a bubble and its zoom', () => {
    const state = fleet(updated(message('m1', [text('Before.'), tool('c1'), text('After.')]), NOW - 1000));
    const { rerender } = render(<Feed state={state} />);
    expect([...bubbleOf(/Before/).querySelectorAll('p')].map((p) => p.textContent)).toEqual([expect.stringMatching(/\S Before\.$/), 'After.']);
    expect(bubbleOf(/Before/).querySelectorAll('[class*=messageTime]')).toHaveLength(1);

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
    // A day on, the time leading the same words gains its date.
    rerender(<Feed state={twoBlocks()} live={RUNNING} now={NOW + 86_400_000} />);
    expect(mainBubble().querySelector('[class*=messageTime]')).toHaveTextContent(new Date(NOW - 1000).toLocaleDateString([], { month: 'short', day: 'numeric' }));
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
    expect(screen.queryByRole('dialog')).toBeNull();
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
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});

/**
 * Every bubble as a 100px scroller over `tall()` px of words (the card roomy
 * around it), sliding text in with `glide` unless motion is reduced.
 */
const SCROLLER = '[data-bubble] > div';
const scrolling = (tall: () => number, { reduce = false, firstBlockHeight }: { reduce?: boolean; firstBlockHeight?: number } = {}) => {
  const glide = vi.fn();
  Object.defineProperty(HTMLElement.prototype, 'animate', { value: glide, configurable: true });
  vi.stubGlobal('matchMedia', (query: string) => ({ matches: reduce && query.includes('reduce') }));
  const spies = [
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.matches(SCROLLER) ? 100 : 1000;
    }),
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
      if (firstBlockHeight !== undefined && this.matches('[class*=speechText]')) return firstBlockHeight;
      return this.parentElement?.matches(SCROLLER) ? tall() : 0;
    }),
    // A slide's transform stretches what scrolls past the words' own height.
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.matches(SCROLLER) ? tall() + (glide.mock.calls.length > 0 ? 60 : 0) : 0;
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

describe("a bubble's time", () => {
  const DAY_MS = 86_400_000;
  const clock = (at: number) => new Date(at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  const timeOf = (bubble: HTMLElement) => bubble.querySelector<HTMLElement>('[class*=messageTime]')!;

  it('shows when its message was said before its words, inside the scrolling area', () => {
    render(<Feed state={saying('Shipped.')} />);
    const bubble = bubbleOf(/Shipped/);
    const time = timeOf(bubble);
    expect(time).toHaveTextContent(new RegExp(`^${clock(NOW - 1000)}$`));
    expect(time.closest('[class*=bubbleText]')).toBe(bubble.querySelector('[class*=bubbleText]'));
    expect(time.parentElement).toHaveTextContent(new RegExp(`^${clock(NOW - 1000)} Shipped\\.$`));
  });

  // jsdom lays nothing out: the one line, the shared baseline and the bubble's width rest on the time being the first line's own text.
  it('starts its first line, inside the first paragraph, so a short message keeps one line', () => {
    render(<Feed state={saying('ok, fix it')} />);
    const time = timeOf(bubbleOf(/fix it/));
    expect(time.parentElement!.tagName).toBe('P');
    expect(time.parentElement!.firstChild).toBe(time);
    expect(time.parentElement).toHaveTextContent(new RegExp(`^${clock(NOW - 1000)} ok, fix it$`));
  });

  it('takes a line of its own above a message that starts with a list or a quote', () => {
    render(<Feed state={saying('- first\n- second\n\n> quoted')} />);
    const time = timeOf(bubbleOf(/first/));
    expect(time.parentElement!.tagName).toBe('P');
    expect(time.parentElement).toHaveTextContent(new RegExp(`^${clock(NOW - 1000)}$`));
    expect(time.parentElement!.nextElementSibling!.tagName).toBe('UL');
  });

  it('leads a message that starts with raw HTML, which is drawn as its text', () => {
    render(<Feed state={saying('<!-- note -->\n\nHello')} />);
    const time = timeOf(bubbleOf(/Hello/));
    expect(time.closest('[class*=speechText]')).toHaveTextContent(new RegExp(`^${clock(NOW - 1000)} ?<!-- note -->`));
  });

  it('keeps each underlying message time ahead of its text in a shared bubble', () => {
    const firstAt = NOW - 20 * 60_000;
    const secondAt = NOW - 1000;
    const state = fleet(
      updated(message('m1', [text('First, the leaks.'), tool('c1')]), firstAt),
      updated(message('m2', [text('After that, the fix.'), tool('c2')]), secondAt)
    );
    render(<Feed state={state} />);
    const bubble = mainBubble();
    const blocks = [...bubble.querySelectorAll<HTMLElement>('[class*=speechText]')];
    expect(blocks).toHaveLength(2);
    expect(blocks.map((block) => block.querySelector('[class*=messageTime]')?.textContent)).toEqual([clock(firstAt), clock(secondAt)]);
    expect(blocks.map((block) => block.querySelector('p')?.textContent)).toEqual([`${clock(firstAt)} First, the leaks.`, `${clock(secondAt)} After that, the fix.`]);
    expect(blocks.every((block) => block.closest('[class*=bubbleText]') !== null)).toBe(true);

    fireEvent.click(bubble);
    const zoom = screen.getByRole('dialog', { name: 'Message from Main' });
    const zoomMessages = [...zoom.querySelectorAll<HTMLElement>('[class*=zoomMessage]')];
    expect(zoomMessages.map((block) => block.querySelector('[class*=messageTime]')?.textContent)).toEqual([clock(firstAt), clock(secondAt)]);
    expect(zoomMessages.map((block) => block.querySelector('p')?.textContent)).toEqual(['First, the leaks.', 'After that, the fix.']);
  });

  it('adds the date to a message from an earlier day', () => {
    const at = NOW - 2 * DAY_MS;
    render(<Feed state={fleet(updated(message('m1', [text('Old news.'), tool('c1')]), at))} />);
    const time = timeOf(bubbleOf(/Old news/));
    expect(time).toHaveTextContent(new Date(at).toLocaleDateString([], { month: 'short', day: 'numeric' }));
    expect(time).toHaveTextContent(clock(at));
  });

  it('names a completed result beside its time for sighted and screen reader users', () => {
    const completed = { id: 't-done', workspaceId: WS, title: 'Review code', status: 'completed', sessionId: 'task-session', parentTaskId: null, cycleNo: 1n,
      assignedAgentDisplayName: 'Reviewer', assignedAgentDefinitionId: 'def-r', assignedToWorkspaceAgentId: 'wa-r', resultSummary: 'Review complete',
      createdAt: BigInt(NOW - 3000), updatedAt: BigInt(NOW - 1000), completedAt: BigInt(NOW - 1000) } as unknown as WorkspaceTaskResponse;
    render(<Feed state={saying('Shipped.')} live={{ runs: {}, tasks: { [WS]: { completed } } }} />);
    const bubble = bubbleOf(/Review complete/);
    expect(timeOf(bubble)).toHaveTextContent(`${clock(NOW - 1000)} · done`);
    fireEvent.click(bubble);
    expect(screen.getByRole('dialog', { name: 'Message from Reviewer' })).toHaveTextContent(`${clock(NOW - 1000)} · done`);
  });

  it('shows a failed result and its time together without a status glyph', () => {
    const failed = { id: 't-failed', workspaceId: WS, title: 'Review code', status: 'failed', sessionId: 'task-session', parentTaskId: null, cycleNo: 1n,
      assignedAgentDisplayName: 'Reviewer', assignedAgentDefinitionId: 'def-r', assignedToWorkspaceAgentId: 'wa-r', error: 'Session limit reached',
      createdAt: BigInt(NOW - 3000), updatedAt: BigInt(NOW - 1000), completedAt: BigInt(NOW - 1000) } as unknown as WorkspaceTaskResponse;
    render(<Feed state={saying('Shipped.')} live={{ runs: {}, tasks: { [WS]: { failed } } }} />);
    const bubble = bubbleOf(/Session limit reached/);
    const time = timeOf(bubble);
    expect(time).toHaveTextContent(`${clock(NOW - 1000)} · failed`);
    expect(time).toHaveAttribute('data-outcome', 'failed');
    expect(time.closest('[class*=bubbleText]')).not.toBeNull();
    expect(bubble.querySelector('svg path[d="M6 6l12 12M18 6L6 18"]')).toBeNull();
  });
});

describe('a bubble, scrolling', () => {
  it('follows the full height of two messages in one bubble', () => {
    let height = 220;
    const { restore } = scrolling(() => height, { firstBlockHeight: 80 });
    const first = updated(message('m1', [text('First, the leaks.'), tool('c1')]), NOW - 2000);
    const second = (content: string) => updated(message('m2', [text(content), tool('c2')]), NOW - 1000);
    try {
      const { rerender } = render(<Feed state={fleet(first, second('After that, the fix.'))} />);
      const scroller = () => mainBubble().querySelector<HTMLElement>(SCROLLER)!;
      expect(scroller().querySelectorAll('[class*=speechText]')).toHaveLength(2);
      expect(scroller().scrollTop).toBe(120);

      height = 280;
      rerender(<Feed state={fleet(first, second('After that, the fix. Then the tests.'))} />);
      expect(scroller().scrollTop).toBe(180);
    } finally {
      restore();
    }
  });

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

  it('shows its end while it is the newest, run going or done, and its start once another follows, whoever said it', () => {
    const { restore } = scrolling(() => 200);
    const scrollOf = (words: RegExp) => bubbleOf(words).querySelector<HTMLElement>(SCROLLER)!.scrollTop;
    try {
      const { rerender } = render(writing());
      expect(scrollOf(/First, the leaks/)).toBe(100);
      rerender(<Feed state={twoBlocks()} />);
      expect(scrollOf(/First, the leaks/)).toBe(100);

      const asked = applyUserSent(twoBlocks(), WS, MAIN, { ...message('u1', [text('And the docs?')]), role: 'user' }, NOW);
      rerender(<Feed state={asked} now={NOW + 2000} />);
      expect(scrollOf(/First, the leaks/)).toBe(0);
      expect(scrollOf(/And the docs/)).toBe(100);

      rerender(<Feed state={applyAssistantEvent(asked, updated(message('m2', [text('Docs next.'), tool('c3')]), NOW + 1000))} now={NOW + 2000} />);
      expect(scrollOf(/First, the leaks/)).toBe(0);
      expect(scrollOf(/And the docs/)).toBe(0);
      expect(scrollOf(/Docs next/)).toBe(100);
    } finally {
      restore();
    }
  });

  it.each([
    ['yours', { kind: 'user' }],
    ["an agent's", { kind: 'agent', agent: { workspaceAgentId: 'wa-s', agentDefinitionId: 'def-s', displayName: 'Scout', isDefault: false }, sessionId: 'task-s', runId: null, replyAddress: null }],
  ])("%s follows its end as its words grow, sliding them in, like Main's; scrolled up, it stays", (_, origin) => {
    let tall = 200;
    const { glide, restore } = scrolling(() => tall);
    const said = { ...message('w1', [text('First words')]), role: 'user', origin, createdAt: BigInt(NOW - 1000) } as AssistantMessage;
    const cut = seedFleet(EMPTY_FLEET, [snapshotWith({ message: said, runId: null, truncated: true })]);
    const grown = applyWholeMessage(cut, WS, { ...said, content: [text('First words, then more.')] });
    const scroller = () => bubbleOf(/First words/).querySelector<HTMLElement>(SCROLLER)!;
    try {
      const { rerender, unmount } = render(<Feed state={cut} />);
      expect(scroller().scrollTop).toBe(100);
      tall = 260;
      rerender(<Feed state={grown} />);
      expect(scroller()).toHaveTextContent('then more.');
      expect(scroller().scrollTop).toBe(160);
      expect(glide).toHaveBeenCalledTimes(1);
      unmount();

      tall = 200;
      const again = render(<Feed state={cut} />);
      scroller().scrollTop = 40;
      tall = 260;
      again.rerender(<Feed state={grown} />);
      expect(scroller().scrollTop).toBe(40);
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

  it('moves one line a wheel notch, newest or older; leaves a trackpad, Ctrl and sideways to the browser', () => {
    const { restore } = scrolling(() => 200);
    /** A wheel event of `wheelDeltaY` (120 a notch) on the words; false if it was taken. */
    const wheel = (wheelDeltaY: number, init: WheelEventInit = {}) => {
      const e = new WheelEvent('wheel', { bubbles: true, cancelable: true, deltaY: -wheelDeltaY, ...init });
      Object.defineProperty(e, 'wheelDeltaY', { value: wheelDeltaY });
      return words().dispatchEvent(e);
    };
    try {
      const { rerender } = render(writing());
      words().style.lineHeight = '15px';
      expect(words().scrollTop).toBe(100);
      expect(wheel(120)).toBe(false);
      expect(words().scrollTop).toBe(85);
      expect(wheel(240)).toBe(false);
      expect(words().scrollTop).toBe(55);
      expect(wheel(-120)).toBe(false);
      expect(words().scrollTop).toBe(70);
      expect(wheel(36)).toBe(true);
      expect(wheel(0, { deltaX: 40 })).toBe(true);
      expect(wheel(120, { ctrlKey: true })).toBe(true);
      expect(wheel(120, { cancelable: false })).toBe(true);
      expect(words().scrollTop).toBe(70);

      rerender(<Feed state={applyUserSent(twoBlocks(), WS, MAIN, { ...message('u1', [text('And the docs?')]), role: 'user' }, NOW)} now={NOW + 2000} />);
      expect(words().scrollTop).toBe(0);
      expect(wheel(-120)).toBe(false);
      expect(words().scrollTop).toBe(15);
    } finally {
      restore();
    }
  });

  it('leaves a click on its scrollbar to the scrollbar', () => {
    const { restore } = scrolling(() => 200);
    try {
      render(writing());
      fireEvent.click(words(), { clientX: 203 });
      expect(screen.queryByRole('dialog')).toBeNull();
      fireEvent.click(words(), { clientX: 120 });
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    } finally {
      restore();
    }
  });
});

describe('a link in a bubble', () => {
  const agent = { kind: 'agent', agent: { workspaceAgentId: 'wa-s', agentDefinitionId: 'def-s', displayName: 'Scout', isDefault: false }, sessionId: 'task-s', runId: null, replyAddress: null };
  const created = (m: AssistantMessage, at: number) => event({ type: 'message_created', payload: { message: m } } as AssistantUiEvent, at);
  /** Main's, yours and an agent's bubble, each with a bare URL. */
  const state = () =>
    applyAssistantEvent(
      applyUserSent(saying('First, the leaks: https://github.com/clairun/clai/pull/236'), WS, MAIN, { ...message('u1', [text('Mine: https://example.com/yours')]), role: 'user' }, NOW),
      created({ ...message('s1', [text('Scout: https://example.com/scout')]), role: 'user', origin: agent } as AssistantMessage, NOW + 1)
    );
  /** What the opener plugin does on `window`: open a new-tab link clicked, unless something prevented it. */
  const opened: string[] = [];
  const keys: string[] = [];
  const opener = (e: MouseEvent) => {
    const link = e.target instanceof Element ? e.target.closest<HTMLAnchorElement>('a[target=_blank]') : null;
    if (!link || e.defaultPrevented) return;
    e.preventDefault();
    opened.push(link.href);
  };
  const key = (e: KeyboardEvent) => keys.push(e.key);
  let height: { mockRestore: () => void };
  beforeEach(() => {
    opened.length = 0;
    keys.length = 0;
    window.addEventListener('click', opener);
    window.addEventListener('keydown', key);
    height = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(1000);
  });
  afterEach(() => {
    window.removeEventListener('click', opener);
    window.removeEventListener('keydown', key);
    height.mockRestore();
  });

  it.each([
    ["Main's bubble", 'https://github.com/clairun/clai/pull/236'],
    ['your bubble', 'https://example.com/yours'],
    ["an agent's bubble", 'https://example.com/scout'],
  ])('in %s opens on a click, through to the opener, and does not zoom the bubble', (_, href) => {
    render(<Feed state={state()} now={NOW + 2000} />);
    fireEvent.click(screen.getByRole('link', { name: href }));
    expect(opened).toEqual([href]);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('opens on Enter too: the key and the click it makes both go through, and the bubble does not zoom', () => {
    render(<Feed state={state()} now={NOW + 2000} />);
    const link = screen.getByRole('link', { name: 'https://example.com/scout' });
    link.focus();
    // Not prevented: the browser turns it into a click on the link.
    expect(fireEvent.keyDown(link, { key: 'Enter' })).toBe(true);
    expect(keys).toEqual(['Enter']);
    fireEvent.click(link);
    expect(opened).toEqual(['https://example.com/scout']);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('in a zoom opens on a click, through to the opener, and the zoom stays open', () => {
    render(<Feed state={state()} now={NOW + 2000} />);
    fireEvent.click(bubbleOf(/Scout:/));
    const zoom = screen.getByRole('dialog');
    fireEvent.click(within(zoom).getByRole('link', { name: 'https://example.com/scout' }));
    expect(opened).toEqual(['https://example.com/scout']);
    expect(screen.getByRole('dialog')).toBe(zoom);
  });
});

describe('a zoom, as more is said on its card', () => {
  const yours = (state: FleetState) => applyUserSent(state, WS, MAIN, { ...message('u1', [text('And the docs?')]), role: 'user' }, NOW);
  let height: { mockRestore: () => void };
  beforeEach(() => {
    height = vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(1000);
  });
  afterEach(() => height.mockRestore());

  it('closes when you speak on its card, so your word shows; a click zooms again', () => {
    const { rerender } = render(<Feed state={twoBlocks()} />);
    const bubble = mainBubble();
    fireEvent.click(bubble);
    expect(screen.getByRole('dialog', { name: 'Message from Main' })).toBeInTheDocument();
    rerender(<Feed state={yours(twoBlocks())} />);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.getByText('And the docs?')).toBeInTheDocument();
    fireEvent.click(bubble);
    expect(screen.getByRole('dialog', { name: 'Message from Main' })).toBeInTheDocument();
  });

  it('stays as Main or an agent says more', () => {
    const done = { id: 't1', workspaceId: WS, title: 'Draft the release notes', status: 'completed', resultSummary: 'Notes drafted.', sessionId: 'task-session',
      parentTaskId: null, cycleNo: 1n, assignedAgentDisplayName: 'Writer', assignedAgentDefinitionId: 'def-w', assignedToWorkspaceAgentId: 'wa-w',
      createdAt: BigInt(NOW + 500), updatedAt: BigInt(NOW + 800), completedAt: BigInt(NOW + 800) } as unknown as WorkspaceTaskResponse;
    const { rerender } = render(<Feed state={twoBlocks()} />);
    fireEvent.click(mainBubble());
    const more = applyAssistantEvent(twoBlocks(), updated(message('m2', [text('Docs next.'), tool('c3')]), NOW + 1000));
    rerender(<Feed state={more} live={{ runs: {}, tasks: { [WS]: { t1: done } } }} now={NOW + 2000} />);
    expect(screen.getByText('Notes drafted.')).toBeInTheDocument();
    expect(screen.getByText('Docs next.')).toBeInTheDocument();
    expect(screen.getByRole('dialog', { name: 'Message from Main' })).toHaveTextContent('First, the leaks.');
  });

  it("leaves another card's zoom open", () => {
    const WS2 = 'ws-2';
    const other = (state: FleetState) => {
      const said = { ...message('o1', [text('Other card words.'), tool('c9')]), sessionId: 'main-2' };
      return applyAssistantEvent(applyUserSent(state, WS2, 'main-2', null, 0), { ...updated(said, NOW - 1000), workspaceId: WS2, sessionId: 'main-2' });
    };
    const Cards = ({ state }: { state: FleetState }) =>
      [WS, WS2].map((id) => (
        <FleetFeed key={id} workspaceId={id} feed={cardFeed(state.cards[id]!, IDLE, NOW)} shouts={new Set()} now={NOW} faces={castFor({}, null)} onOpen={() => {}} />
      ));
    const { rerender } = render(<Cards state={other(twoBlocks())} />);
    fireEvent.click(mainBubble());
    fireEvent.click(bubbleOf(/Other card words/));
    expect(screen.getAllByRole('dialog')).toHaveLength(2);
    rerender(<Cards state={yours(other(twoBlocks()))} />);
    expect(screen.getByRole('dialog')).toHaveTextContent('Other card words.');
  });

  it('stays closed as the word that closed it is taken back or ages out of the feed', () => {
    const { rerender } = render(<Feed state={twoBlocks()} />);
    fireEvent.click(mainBubble());
    rerender(<Feed state={yours(twoBlocks())} />);
    expect(screen.queryByRole('dialog')).toBeNull();
    const deleted = event({ type: 'message_deleted', payload: { message_id: 'u1' } } as AssistantUiEvent, NOW + 1);
    rerender(<Feed state={applyAssistantEvent(yours(twoBlocks()), deleted)} />);
    expect(screen.queryByRole('dialog')).toBeNull();
    const replies = Array.from({ length: 30 }, (_, i) => ({ ...updated(message(`r${i}`, [text(`Reply ${i}.`), tool('c')]), NOW + 10 + i), runId: `run-${i}` }));
    const aged = replies.reduce(applyAssistantEvent, yours(twoBlocks()));
    rerender(<Feed state={aged} />);
    expect(screen.queryByText('And the docs?')).toBeNull();
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});

describe('words selected in the feed', () => {
  /** Main, you, Main: an older bubble of Main's, yours, and Main's newest. */
  const asked = () => {
    const state = applyUserSent(twoBlocks(), WS, MAIN, { ...message('u1', [text('And the docs?')]), role: 'user' }, NOW - 500);
    return applyAssistantEvent(state, updated(message('m2', [text('Docs next.'), tool('c3')]), NOW));
  };
  /** Selects the words in `node`, as a drag over them does; letting go clicks them. */
  const select = (node: HTMLElement) => {
    const range = document.createRange();
    range.selectNodeContents(node);
    getSelection()!.removeAllRanges();
    getSelection()!.addRange(range);
    fireEvent.click(node);
    return getSelection()!.toString();
  };
  const zoomed = () => screen.queryByRole('dialog') !== null;
  // Room for three bubbles.
  const sizes: { mockRestore: () => void }[] = [];
  beforeEach(() => {
    sizes.push(
      vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(200),
      vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.tagName === 'P' ? 20 : 40;
      })
    );
  });
  afterEach(() => {
    sizes.splice(0).forEach((size) => size.mockRestore());
    getSelection()!.removeAllRanges();
  });

  it("stay selected in Main's bubbles, older or newest, which a plain click zooms", () => {
    render(<Feed state={asked()} />);
    for (const words of ['First, the leaks.', 'Docs next.']) {
      expect(select(screen.getByText(words))).toMatch(new RegExp(`\\S ${words}$`));
      expect(zoomed()).toBe(false);
    }
    // A shift-click here extends a selection begun in another bubble.
    getSelection()!.setBaseAndExtent(screen.getByText('Docs next.'), 0, screen.getByText('First, the leaks.'), 0);
    fireEvent.click(screen.getByText('First, the leaks.'));
    expect(zoomed()).toBe(false);
    // Words selected in another bubble leave a plain click on this one a press.
    getSelection()!.selectAllChildren(screen.getByText('Docs next.'));
    fireEvent.click(screen.getByText('First, the leaks.'));
    expect(screen.getByRole('dialog', { name: 'Message from Main' })).toHaveTextContent('First, the leaks.');
  });

  it('stay selected in your bubble, which a plain click zooms', () => {
    render(<Feed state={asked()} />);
    expect(select(screen.getByText('And the docs?'))).toMatch(/\S And the docs\?$/);
    expect(screen.queryByRole('dialog')).toBeNull();
    getSelection()!.removeAllRanges();
    fireEvent.click(screen.getByText('And the docs?'));
    expect(screen.getByRole('dialog', { name: 'Message from You' })).toBeInTheDocument();
  });
});

describe('a task in the feed', () => {
  const queued = { id: 't1', workspaceId: WS, title: 'Draft the release notes', status: 'queued', sessionId: 'task-session', parentTaskId: null, cycleNo: 1n,
    assignedAgentDisplayName: 'Writer', assignedAgentDefinitionId: 'def-w', assignedToWorkspaceAgentId: 'wa-w', createdAt: BigInt(NOW - 1000), updatedAt: BigInt(NOW - 1000) } as unknown as WorkspaceTaskResponse;

  it('says a queued task is next', () => {
    render(<Feed state={saying('First, the leaks.')} live={{ runs: {}, tasks: { [WS]: { t1: queued } } }} />);
    expect(bubbleOf(/Draft the release notes/)).toHaveTextContent(/· next/);
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
      // Main's word, not yours: yours would close the zoom.
      rerender(<Feed state={fleet(...said, updated(message('m3', [text('Last.'), tool('c3')]), NOW))} />);
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
