import type { TaskStatus, WorkspaceTaskResponse } from '../generated/bindings';
import type { ImagePart } from '../assistant/imageParts';
import { taskDisplayStatus } from '../utils/taskDisplay';
import {
  agentRef,
  assigneeOf,
  isTaskLive,
  mainRunning,
  workspaceTasks,
  type FleetAgentRef,
  type FleetCard,
  type FleetLive,
  type FleetSaid,
} from './fleetModel';

// ── A miniature: the workspace's conversation, as highlights ─────────────

const DAY_MS = 24 * 60 * 60 * 1000;
/** A task that just ended keeps the panel's caption "Just finished" this long. */
export const FRESH_MS = 10 * 60 * 1000;
/** A card whose last word is older than this is stale: its drawing fades. */
export const STALE_MS = DAY_MS;
/** Older highlights collapsed to one-line rows above the bubbles; older ones hide. */
export const ROWS = 2;
/** Highlights a miniature has; rows that don't fit hide from the top. */
export const FEED_LIMIT = 16;
const SENTENCE_CHARS = 200;
/**
 * How a task's assignment message starts (`assignment_text` in
 * `workspace_tasks.rs`): its task's creation says it, by title.
 */
const ASSIGNMENT_PREFIX = 'You have been assigned a workspace-local task';

export type FleetSpeaker = { kind: 'you' } | { kind: 'main' } | { kind: 'agent'; agent: FleetAgentRef };

type FleetOutcome = 'done' | 'failed' | 'stopped';

/** One thing said in the workspace: who to whom, what, and how a task ended. */
export interface FleetLine {
  key: string;
  at: number;
  from: FleetSpeaker;
  to: FleetSpeaker;
  /** As plain text, for a one-line row. */
  text: string;
  /** As said, in markdown, for a bubble and the zoom. */
  markdown: string;
  /** The pictures attached to the message. */
  images: readonly ImagePart[];
  /** The message said, to fetch in full when `truncated`; `null` for a task's own line. */
  message: { sessionId: string; id: string; truncated: boolean } | null;
  /** The first sentence, for a one-line row. */
  headline: string;
  /** A task's result. */
  outcome: FleetOutcome | null;
  /** A task handed over and not ended yet: its agent is (or will be) working. */
  live: 'running' | 'queued' | null;
}

type FleetMood = 'working' | 'fresh' | 'idle' | 'stale';

/**
 * What a miniature says: the caption says when, the lines say what, oldest
 * first. Lines come from events and tasks only, so polling that changes
 * nothing changes nothing here; the mood and caption age with the clock.
 */
export interface FleetFeed {
  mood: FleetMood;
  caption: FleetCaption;
  lines: FleetLine[];
  /** `FleetCard.youSaidAt`: a word since a zoom opened closes it, so your word shows. */
  youSaidAt: number;
}

/** One markdown line as plain text: no block markers, emphasis or link targets. */
const plainLine = (line: string): string =>
  line
    .replace(/^\s*(?:#{1,6}\s+|>\s?|[-*+]\s+|\d+\.\s+)/, '')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\*\*|__|`|\*/g, '')
    .trim();

/** A markdown text as one plain paragraph, for a one-line row. */
export const plainText = (markdown: string | null | undefined): string =>
  (markdown ?? '').split('\n').map(plainLine).filter(Boolean).join(' ');

/**
 * The first sentence of a markdown text, as plain text: the headline of a
 * message or a result. Cut at the first sentence end, or at the first line.
 */
export const firstSentence = (markdown: string | null | undefined): string => {
  const plain = (markdown ?? '').split('\n').map(plainLine).find(Boolean) ?? '';
  const end = plain.search(/[.!?…](?=\s|$)/);
  const sentence = end >= 0 ? plain.slice(0, end + 1) : plain;
  return sentence.length > SENTENCE_CHARS ? `${sentence.slice(0, SENTENCE_CHARS - 1)}…` : sentence;
};

/**
 * A panel's caption box: when, not what. Which caption is an event's doing;
 * its words are relative and are worded against a clock (`captionText`), so
 * they can age by the minute while the panel itself waits for events.
 */
export type FleetCaption =
  | { kind: 'working'; count: number }
  | { kind: 'fresh' | 'idle' | 'stale' | 'waiting'; at: number }
  | { kind: 'empty' };

const MINUTE_MS = 60_000;

/** The time since `at`, in whole minutes, hours or days: `12 min`, `3 h`, `1 day`, `2 days`. */
const span = (at: number, clock: number): string => {
  const minutes = Math.floor((clock - at) / MINUTE_MS);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h`;
  const days = Math.floor(hours / 24);
  return days === 1 ? '1 day' : `${days} days`;
};

/** `just now`, `12 min ago`, `3 h ago`, `yesterday`, `3 days ago`. */
export const ago = (at: number, clock: number): string => {
  if (clock - at < MINUTE_MS) return 'just now';
  const since = span(at, clock);
  return since === '1 day' ? 'yesterday' : `${since} ago`;
};

/** How long something has waited: `under a minute`, `12 min`, `3 h`, `2 days`. */
const waited = (at: number, clock: number): string => (clock - at < MINUTE_MS ? 'under a minute' : span(at, clock));

const capitalize = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

export const captionText = (caption: FleetCaption, clock: number): string => {
  switch (caption.kind) {
    case 'working':
      return caption.count > 1 ? `Now · ${caption.count} at once` : 'Now';
    case 'fresh':
      return clock - caption.at < MINUTE_MS ? 'Just now' : `Just finished · ${ago(caption.at, clock)}`;
    case 'idle':
      return `Last word · ${ago(caption.at, clock)}`;
    case 'stale':
      return capitalize(ago(caption.at, clock));
    case 'waiting':
      return `Waiting on you · ${waited(caption.at, clock)}`;
    case 'empty':
      return 'Nothing yet';
  }
};

/** `14:05` on `now`'s day, `Oct 16` before. */
export const shortTime = (at: number, now: number): string => {
  const date = new Date(at);
  return date.toDateString() === new Date(now).toDateString()
    ? date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : date.toLocaleDateString([], { month: 'short', day: 'numeric' });
};

const YOU: FleetSpeaker = { kind: 'you' };
const MAIN: FleetSpeaker = { kind: 'main' };
const speaker = (agent: FleetAgentRef | null): FleetSpeaker => (agent ? { kind: 'agent', agent } : MAIN);
const OUTCOMES: Partial<Record<TaskStatus, FleetOutcome>> = { completed: 'done', failed: 'failed', stopped: 'stopped' };

/** How a task ended, in a few words. */
const resultOf = (task: WorkspaceTaskResponse): string =>
  (task.status === 'failed' ? task.error : task.resultSummary)?.trim() ||
  (task.status === 'failed' ? taskDisplayStatus(task).label : task.title);

type LineParts = Pick<FleetLine, 'key' | 'at' | 'from' | 'to' | 'markdown'> &
  Partial<Pick<FleetLine, 'images' | 'outcome' | 'live' | 'message'>>;

const line = ({ markdown, images = [], outcome = null, live = null, message = null, ...said }: LineParts): FleetLine => ({
  ...said,
  text: plainText(markdown),
  markdown,
  images,
  message,
  headline: firstSentence(markdown),
  outcome,
  live,
});

const messageOf = (said: FleetSaid): FleetLine['message'] => ({ sessionId: said.sessionId, id: said.id, truncated: said.truncated });

const fromSaid = (said: FleetSaid, from: FleetSpeaker, to: FleetSpeaker, outcome: FleetOutcome | null = null) =>
  line({ key: said.id, at: said.at, from, to, markdown: said.text, images: said.images, outcome, message: messageOf(said) });

/** A block of Main's words to you, before `speak` joins it into a bubble. */
interface Stretch {
  key: string;
  at: number;
  run: string | null;
  markdown: string;
  message: FleetLine['message'];
}

const isStretch = (item: FleetLine | Stretch): item is Stretch => 'run' in item;

/** An assistant message's blocks, each at the time it was heard. */
const stretches = (said: FleetSaid): Stretch[] =>
  said.marks.flatMap((mark, i) => {
    const markdown = said.text.slice(i ? said.marks[i - 1]!.end : 0, mark.end).trim();
    return markdown ? [{ key: i ? `${said.id}:${i}` : said.id, at: mark.at, run: said.runId, markdown, message: messageOf(said) }] : [];
  });

/**
 * Lines in time order, Main's blocks joined into bubbles: blocks of one run
 * share a bubble until something else is said, a task is handed over or a
 * question is asked between them. A bubble is keyed by its first block, so
 * it keeps its key as it grows, and is timed by its last.
 */
const speak = (items: readonly (FleetLine | Stretch)[], asked: readonly number[]): FleetLine[] => {
  const lines: FleetLine[] = [];
  let bubble: Stretch[] = [];
  const close = () => {
    const first = bubble[0];
    if (!first) return;
    const message = bubble.every((s) => s.message?.id === first.message?.id) ? first.message : null;
    const markdown = bubble.map((s) => s.markdown).join('\n\n');
    lines.push(line({ key: first.key, at: bubble.at(-1)!.at, from: MAIN, to: YOU, markdown, message }));
    bubble = [];
  };
  for (const item of items) {
    if (!isStretch(item)) {
      close();
      lines.push(item);
      continue;
    }
    const last = bubble.at(-1);
    if (last && (item.run === null || item.run !== last.run || asked.some((t) => t > last.at && t <= item.at))) close();
    bubble.push(item);
  }
  close();
  return lines;
};

/**
 * A miniature's highlights, oldest first: the user's messages and Main's
 * words on the main session (`speak`), what agents told each other (Main to a task,
 * a task's report to whoever handed it over), and each task handed over and
 * ended. Tool calls are never said. A task's report is how it ended; its own
 * status says so only when no report has arrived.
 */
export const cardFeed = (card: FleetCard, live: FleetLive, now: number): FleetFeed => {
  const tasks = workspaceTasks(card, live);
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const bySession = new Map(tasks.flatMap((t) => (t.sessionId ? [[t.sessionId, t] as const] : [])));
  const main = card.mainSessionId;
  const ownerOf = (sessionId: string): FleetSpeaker | null => {
    if (sessionId === main) return MAIN;
    const task = bySession.get(sessionId);
    return task ? speaker(assigneeOf(card, task)) : null;
  };

  const lines: (FleetLine | Stretch)[] = [];
  const reported = new Set<string>();
  for (const said of card.said) {
    const to = said.role === 'assistant' ? (said.sessionId === main ? YOU : null) : ownerOf(said.sessionId);
    const origin = said.origin;
    if (!to) continue;
    if (!origin) {
      lines.push(...stretches(said));
    } else if (origin.kind === 'user') {
      lines.push(fromSaid(said, YOU, to));
    } else if (origin.kind === 'agent') {
      if (said.text.startsWith(ASSIGNMENT_PREFIX)) continue;
      lines.push(fromSaid(said, speaker(agentRef(origin.agent)), to));
    } else {
      reported.add(`${origin.taskId}:${origin.cycleNo}`);
      lines.push(fromSaid(said, speaker(agentRef(origin.agent)), to, OUTCOMES[origin.status] ?? null));
    }
  }
  for (const task of tasks) {
    const agent = speaker(assigneeOf(card, task));
    const parent = task.parentTaskId ? byId.get(task.parentTaskId) : undefined;
    const boss = parent ? speaker(assigneeOf(card, parent)) : MAIN;
    const state = task.status === 'queued' ? 'queued' : isTaskLive(task.status) ? 'running' : null;
    lines.push(line({ key: `task:${task.id}`, at: Number(task.createdAt), from: boss, to: agent, markdown: task.title, live: state }));
    const outcome = OUTCOMES[task.status];
    if (outcome && !reported.has(`${task.id}:${task.cycleNo}`)) {
      lines.push(line({ key: `ended:${task.id}`, at: Number(task.completedAt ?? task.updatedAt), from: agent, to: boss, markdown: resultOf(task), outcome }));
    }
  }
  lines.sort((a, b) => a.at - b.at);
  const shown = speak(lines, card.asked).slice(-FEED_LIMIT);

  const working = tasks.filter((t) => isTaskLive(t.status)).length;
  const ended = [...shown].reverse().find((l) => l.outcome === 'done' || l.outcome === 'failed');
  const lastWord = shown.at(-1)?.at ?? card.lastActivityAt;
  const mood: FleetMood =
    working > 0 || mainRunning(card, live)
      ? 'working'
      : ended && now - ended.at < FRESH_MS
        ? 'fresh'
        : lastWord > 0 && now - lastWord >= STALE_MS
          ? 'stale'
          : 'idle';
  const caption: FleetCaption =
    mood === 'working'
      ? { kind: 'working', count: working }
      : mood === 'fresh'
        ? { kind: 'fresh', at: ended!.at }
        : lastWord > 0
          ? { kind: mood, at: lastWord }
          : { kind: 'empty' };
  return { mood, caption, lines: shown, youSaidAt: card.youSaidAt };
};

/** Bubbles a miniature draws before it keeps room for a row of what came before. */
export const MIN_BUBBLES = 3;

/**
 * How many of a miniature's highlights are bubbles and how many rows, given
 * each highlight's height as a bubble (oldest first, with its gap) and the
 * height the card has for them. Bubbles fill from the newest as long as they
 * fit; the newest is a bubble even when it doesn't. Past `MIN_BUBBLES`, a
 * bubble also leaves room for one row when older highlights remain. Up to
 * `ROWS` older ones collapse to rows of `rowHeight` above the bubbles, as
 * many as fit in what is left; `rowsEdge` is the rule under the last row.
 * Older ones hide.
 */
export const splitFeed = (
  heights: readonly number[],
  available: number,
  rowHeight: number,
  rowsEdge = 0
): { bubbles: number; rows: number } => {
  const n = heights.length;
  let used = 0;
  let bubbles = 0;
  while (bubbles < n) {
    const next = heights[n - 1 - bubbles]!;
    const reserve = bubbles >= MIN_BUBBLES && n - bubbles > 1 ? rowHeight + rowsEdge : 0;
    if (bubbles > 0 && used + next + reserve > available) break;
    used += next;
    bubbles += 1;
  }
  let rows = Math.min(ROWS, n - bubbles);
  while (rows > 0 && used + rows * rowHeight + rowsEdge > available) rows -= 1;
  return { bubbles, rows };
};
