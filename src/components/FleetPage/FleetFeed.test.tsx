import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import type { AssistantEventEnvelope, AssistantMessage, AssistantUiEvent } from '../../generated/bindings';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => undefined) }));

import FleetFeed from './FleetFeed';
import { castFor } from './FleetFace';
import { EMPTY_FLEET, applyAssistantEvent, applyUserSent, cardFeed, type FleetLive, type FleetState } from '../../fleet/fleetModel';

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

const Feed = ({ state, live = IDLE, onOpen = () => {} }: { state: FleetState; live?: FleetLive; onOpen?: (id: string) => void }) => (
  <FleetFeed workspaceId={WS} feed={cardFeed(state.cards[WS]!, live, NOW)} shouts={new Set()} now={NOW} faces={castFor({}, null)} onOpen={onOpen} />
);

const mainBubble = () => screen.getByText(/First, the leaks/).closest<HTMLElement>('[role=button]')!;
const words = () => mainBubble().querySelector('[data-bubble] > span')!;

describe("Main's bubble", () => {
  it('shows its newest lines while its run goes on, and its first once the run is done', () => {
    const { rerender } = render(<Feed state={twoBlocks()} live={RUNNING} />);
    expect(words()).toHaveAttribute('data-tail');
    expect(words()).toHaveTextContent('First, the leaks. After that, the fix.');
    rerender(<Feed state={twoBlocks()} live={IDLE} />);
    expect(words()).not.toHaveAttribute('data-tail');
  });

  it('opens in place on a click, keeps growing open, and closes on the next click; it never zooms', () => {
    const { rerender } = render(<Feed state={twoBlocks()} live={RUNNING} />);
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'false');
    fireEvent.click(mainBubble());
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'true');
    // Open, it scrolls inside instead of following the newest lines.
    expect(words()).toHaveAttribute('data-open');
    expect(words()).not.toHaveAttribute('data-tail');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    rerender(<Feed state={twoBlocks('After that, the fix. Then the docs.')} live={RUNNING} />);
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'true');
    expect(words()).toHaveTextContent('Then the docs.');

    fireEvent.click(mainBubble());
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'false');
    expect(words()).not.toHaveAttribute('data-open');
    fireEvent.keyDown(mainBubble(), { key: 'Enter' });
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'true');
  });

  it('draws a chart as a chip that opens the workspace, without opening the bubble', () => {
    const onOpen = vi.fn();
    const state = fleet(updated(message('m1', [text('First, the leaks: ![Leaks by day](charts/leaks.vl.json) Worst on Monday.'), tool('c1')]), NOW - 1000));
    render(<Feed state={state} onOpen={onOpen} />);
    const chip = within(mainBubble()).getByRole('button', { name: /Leaks by day/ });
    expect(mainBubble().querySelector('img, canvas, svg[role=graphics-document]')).toBeNull();
    fireEvent.click(chip);
    fireEvent.keyDown(chip, { key: 'Enter' });
    expect(onOpen).toHaveBeenCalledWith(WS);
    expect(mainBubble()).toHaveAttribute('aria-expanded', 'false');
  });
});
