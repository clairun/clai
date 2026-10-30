import type { WorkspaceTaskResponse } from '../generated/bindings';
import { FRESH_MS, firstSentence, plainText, type FleetCaption, type FleetFeed, type FleetLine } from './fleetFeed';
import {
  assigneeOf,
  mainRunning,
  openRequests,
  workspaceTasks,
  type FleetAgentRef,
  type FleetCard,
  type FleetLive,
  type FleetRequest,
} from './fleetModel';

/**
 * How loud a beat still is: a "GO!" or "DONE!" is drawn in full for half of
 * `FRESH_MS`, at half strength for the rest, then not at all.
 */
export type FleetShout = 'full' | 'half' | null;

export const shoutLevel = (at: number, now: number): FleetShout =>
  now - at < FRESH_MS / 2 ? 'full' : now - at < FRESH_MS ? 'half' : null;

/**
 * How loud a panel is drawn: it needs the user, or a task on it just
 * finished well.
 */
export type FleetLoudness = 'needs' | 'done' | null;

export interface FleetPanel {
  card: FleetCard;
  /** `null` while the card shows an open request. */
  feed: FleetFeed | null;
  selected: boolean;
}

interface FleetLoud {
  /** Loudness by workspace id; a card missing here is quiet. */
  cards: Record<string, FleetLoudness>;
  /** Keys of the lines that shout: the newest running task, the loud card's newest result. */
  rows: ReadonlySet<string>;
}

/**
 * What the page draws loud. Every card that needs the user; otherwise at
 * most one, the card with the newest good result. A selected card never is:
 * the user is already looking at it. Of the running tasks, only the newest
 * rushes in; the rest just think.
 */
export const fleetLoudness = (panels: FleetPanel[]): FleetLoud => {
  const cards: Record<string, FleetLoudness> = {};
  let go: FleetLine | undefined;
  let done: { id: string; line: FleetLine } | undefined;
  for (const { card, feed, selected } of panels) {
    if (!selected && openRequests(card).length > 0) cards[card.workspaceId] = 'needs';
    for (const l of feed?.lines ?? []) if (l.live === 'running' && (!go || l.at > go.at)) go = l;
    if (selected || feed?.mood !== 'fresh') continue;
    // A failure is news too, but not a cheer.
    for (const l of feed.lines) if (l.outcome === 'done' && (!done || l.at > done.line.at)) done = { id: card.workspaceId, line: l };
  }
  const rows = new Set(go ? [go.key] : []);
  if (done && Object.keys(cards).length === 0) {
    cards[done.id] = 'done';
    rows.add(done.line.key);
  }
  return { cards, rows };
};

/** A card that needs the user, as the comic draws it. */
export interface FleetRequestScene {
  request: FleetRequest;
  /** Open requests, this one included: they are answered one at a time, oldest first. */
  count: number;
  caption: FleetCaption;
  /** The asking agent, or `null` for Main. */
  asker: FleetAgentRef | null;
}

/**
 * Who made a request; `null` for Main. A question names its session, which is
 * a task's or the Main's; an approval or a grant names its agent, found in the
 * roster, and Main's name is no agent's.
 */
const askerOf = (card: FleetCard, tasks: readonly WorkspaceTaskResponse[], request: FleetRequest): FleetAgentRef | null => {
  if (request.kind !== 'ask') return card.agents.find((a) => a.name === request.agentName) ?? null;
  const task = tasks.find((t) => t.sessionId === request.ask.sessionId);
  return task ? assigneeOf(card, task) : null;
};

/** A card that needs the user: its oldest open request, said by whoever asked it. */
export const requestScene = (card: FleetCard, live: FleetLive): FleetRequestScene | null => {
  const open = openRequests(card);
  const request = open[0];
  if (!request) return null;
  return { request, count: open.length, caption: { kind: 'waiting', at: request.at }, asker: askerOf(card, workspaceTasks(card, live), request) };
};

/** Who a crew member is and what it is up to, as the strip under a card's title draws it. */
export type FleetCrewStatus = 'needs' | 'running' | 'queued' | 'done' | 'idle';

export interface FleetCrewMember {
  seed: string;
  name: string;
  main: boolean;
  status: FleetCrewStatus;
  /** What it asks, works on, waits to start or just finished, in full; `null` when idle. */
  doing: string | null;
  /** The first words of `doing`, drawn beside a running face. */
  words: string | null;
  /** How loud a finish still is; only `done` has one. */
  level: FleetShout;
}

export interface FleetCrew {
  shown: FleetCrewMember[];
  /** Members a long strip leaves out: its "+N". */
  more: FleetCrewMember[];
}

/** Faces a strip draws before it keeps only Main and the busy ones and counts the rest. */
export const MAX_CREW = 6;
const WORDS_CHARS = 22;
const MAIN_SEED = 'main';

/** The first words of a text, cut at a word boundary. */
export const firstWords = (text: string, max = WORDS_CHARS): string => {
  const plain = plainText(text);
  if (plain.length <= max) return plain;
  const cut = plain.slice(0, max);
  const space = cut.lastIndexOf(' ');
  return `${(space > max / 2 ? cut.slice(0, space) : cut).replace(/[\s,.;:]+$/, '')}…`;
};

/**
 * Every member of the workspace, Main first, then the roster, then any agent
 * busy on the card that the roster doesn't know (an agent removed mid-task).
 * Each says one thing, the most urgent: it needs you (an open request it
 * asked), it runs a task (or, Main, a reply), it waits to start one, it just
 * finished one (fading like the other beats, `shoutLevel`), or it is idle.
 * Past `MAX_CREW`, only Main and the busy ones are drawn.
 */
export const cardCrew = (card: FleetCard, live: FleetLive, now: number): FleetCrew => {
  const tasks = workspaceTasks(card, live).sort((a, b) => Number(b.updatedAt) - Number(a.updatedAt));
  const members = new Map<string, FleetAgentRef & { main: boolean }>([[MAIN_SEED, { seed: MAIN_SEED, name: 'Main', main: true }]]);
  for (const agent of card.agents) if (!members.has(agent.seed)) members.set(agent.seed, { ...agent, main: false });
  const join = (agent: FleetAgentRef | null) => {
    if (!agent) return MAIN_SEED;
    if (!members.has(agent.seed)) members.set(agent.seed, { ...agent, main: false });
    return agent.seed;
  };

  const says = new Map<string, { status: FleetCrewStatus; doing: string; level: FleetShout }>();
  const say = (seed: string, status: FleetCrewStatus, doing: string, level: FleetShout = null) => {
    if (!says.has(seed)) says.set(seed, { status, doing, level });
  };
  for (const request of openRequests(card)) say(join(askerOf(card, tasks, request)), 'needs', request.text);
  if (mainRunning(card, live)) {
    const asked = [...card.said].reverse().find((s) => s.sessionId === card.mainSessionId && s.origin?.kind === 'user');
    say(MAIN_SEED, 'running', asked ? `Answering: ${firstSentence(asked.text)}` : 'Answering in the chat');
  }
  for (const task of tasks) if (task.status === 'running' || task.status === 'stopping') say(join(assigneeOf(card, task)), 'running', task.title);
  for (const task of tasks) if (task.status === 'queued') say(join(assigneeOf(card, task)), 'queued', task.title);
  for (const task of tasks) {
    const level = task.status === 'completed' ? shoutLevel(Number(task.completedAt ?? task.updatedAt), now) : null;
    if (level) say(join(assigneeOf(card, task)), 'done', task.title, level);
  }

  const all: FleetCrewMember[] = [...members.values()].map((m) => {
    const what = says.get(m.seed);
    return {
      ...m,
      status: what?.status ?? 'idle',
      doing: what?.doing ?? null,
      words: what?.status === 'running' ? firstWords(what.doing) : null,
      level: what?.level ?? null,
    };
  });
  if (all.length <= MAX_CREW) return { shown: all, more: [] };
  const shown = all.filter((m) => m.main || m.status !== 'idle').slice(0, MAX_CREW - 1);
  return { shown, more: all.filter((m) => !shown.includes(m)) };
};
