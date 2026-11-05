import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

// Every listener stays registered: Tauri's unlisten is async, so a listener
// from before a workspace switch can still fire.
const listeners = vi.hoisted(() => [] as { name: string; handler: (event: { payload: unknown }) => void }[]);
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, handler: (event: { payload: unknown }) => void) => {
    listeners.push({ name, handler });
    return Promise.resolve(() => {});
  }),
}));

import { PendingRequestStack, type PendingRequestSource } from './PendingRequestStack';

interface Ping {
  requestId: string;
  workspaceId: string | null;
  text: string;
}

const list = vi.fn<(workspaceId: string) => Promise<Ping[]>>();
const SOURCE: PendingRequestSource<Ping> = {
  list: (workspaceId) => list(workspaceId),
  requestEvent: 'ping://request',
  resolvedEvent: 'ping://resolved',
  isRequest: (req) => typeof req.text === 'string',
};

const ping = (requestId: string, workspaceId = 'ws-1'): Ping => ({ requestId, workspaceId, text: `ping ${requestId}` });

const fire = (name: string, payload: unknown) =>
  act(() => {
    for (const listener of listeners.filter((l) => l.name === name)) listener.handler({ payload });
  });
const fireRequest = (payload: unknown) => fire(SOURCE.requestEvent, payload);
const fireResolved = (requestId: string) => fire(SOURCE.resolvedEvent, { requestId });

const Stack = ({ workspaceId }: { workspaceId: string | null }) => (
  <PendingRequestStack
    workspaceId={workspaceId}
    source={SOURCE}
    renderCard={(req, card) => (
      <article ref={card.ref} aria-label={req.text}>
        <button type="button" onClick={() => card.onDecided(req.requestId)}>
          Answer {req.requestId}
        </button>
      </article>
    )}
  />
);

const cards = () => screen.queryAllByRole('article').map((card) => card.getAttribute('aria-label'));
const subscribed = () => waitFor(() => expect(listeners.some((l) => l.name === SOURCE.resolvedEvent)).toBe(true));

let scrollIntoView: ReturnType<typeof vi.fn>;

beforeEach(() => {
  listeners.length = 0;
  list.mockReset();
  list.mockResolvedValue([]);
  scrollIntoView = vi.fn();
  Element.prototype.scrollIntoView = scrollIntoView as unknown as Element['scrollIntoView'];
});

afterEach(() => {
  delete (Element.prototype as Partial<Element>).scrollIntoView;
});

describe('PendingRequestStack', () => {
  it('seeds from the pending list of its workspace', async () => {
    list.mockResolvedValue([ping('a'), ping('b')]);
    render(<Stack workspaceId="ws-1" />);
    await waitFor(() => expect(cards()).toEqual(['ping a', 'ping b']));
    expect(list).toHaveBeenCalledWith('ws-1');
  });

  it('adds a request from its event once, even when the seed also had it', async () => {
    list.mockResolvedValue([ping('a')]);
    render(<Stack workspaceId="ws-1" />);
    await waitFor(() => expect(cards()).toEqual(['ping a']));
    fireRequest(ping('a'));
    fireRequest(ping('b'));
    expect(cards()).toEqual(['ping a', 'ping b']);
  });

  it('ignores a request for another workspace, and a payload that is not a request', async () => {
    render(<Stack workspaceId="ws-1" />);
    await subscribed();
    fireRequest(ping('other', 'ws-2'));
    fireRequest({ requestId: 'bad', workspaceId: 'ws-1' });
    fireRequest(null);
    expect(cards()).toEqual([]);
  });

  it('drops the card when the resolved event fires for it (decided elsewhere, run ended or cancelled)', async () => {
    render(<Stack workspaceId="ws-1" />);
    await subscribed();
    fireRequest(ping('a'));
    fireRequest(ping('b'));
    fireResolved('a');
    expect(cards()).toEqual(['ping b']);
  });

  it('drops the card once its decision is accepted', async () => {
    render(<Stack workspaceId="ws-1" />);
    await subscribed();
    fireRequest(ping('a'));
    await userEvent.click(screen.getByRole('button', { name: 'Answer a' }));
    expect(cards()).toEqual([]);
  });

  it('drops the previous workspace cards on a switch, and ignores their late events and seed', async () => {
    let seedOne: (pending: Ping[]) => void = () => {};
    list.mockImplementation((workspaceId) =>
      workspaceId === 'ws-1' ? new Promise((resolve) => (seedOne = resolve)) : Promise.resolve([]),
    );
    const { rerender } = render(<Stack workspaceId="ws-1" />);
    await subscribed();
    fireRequest(ping('a'));
    expect(cards()).toEqual(['ping a']);

    rerender(<Stack workspaceId="ws-2" />);
    expect(cards()).toEqual([]);
    // The ws-1 listener is still registered and its workspace filter passes.
    fireRequest(ping('late', 'ws-1'));
    await act(async () => seedOne([ping('seeded', 'ws-1')]));
    expect(cards()).toEqual([]);

    fireRequest(ping('c', 'ws-2'));
    expect(cards()).toEqual(['ping c']);
  });

  it('draws nothing and subscribes to nothing without a workspace', () => {
    render(<Stack workspaceId={null} />);
    expect(cards()).toEqual([]);
    expect(list).not.toHaveBeenCalled();
    expect(listeners).toEqual([]);
  });

  it('scrolls the first card into view on the first arrival only', async () => {
    render(<Stack workspaceId="ws-1" />);
    await subscribed();
    fireRequest(ping('a'));
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(scrollIntoView.mock.contexts[0]).toBe(screen.getByRole('article', { name: 'ping a' }));
    fireRequest(ping('b'));
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
  });
});
