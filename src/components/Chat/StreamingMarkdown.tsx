import React, { memo, useEffect, useMemo, useRef, useState } from 'react';
import MarkdownMessage, { MarkdownBlock } from './MarkdownMessage';
import styles from './MarkdownMessage.module.css';

/**
 * StreamingMarkdown
 *
 * Wraps MarkdownMessage with two streaming-specific behaviors so the chat
 * experience feels smooth instead of "popping" in provider-sized bursts:
 *
 *   1. A RAF-driven typewriter buffer that paces the displayed substring
 *      toward the accumulated string. Providers ship deltas in 5-50 char
 *      chunks; without smoothing the UI grows in visible blocks. The
 *      buffer caches up exponentially when far behind so very large bursts
 *      don't feel artificially slow.
 *
 *   2. A "stable preview" of partial markdown — unclosed code fences are
 *      auto-closed, unmatched inline backticks and partial link syntax
 *      are stripped from the tail. The point is to prevent layout
 *      flicker where a block of literal text suddenly turns into a code
 *      block / link / inline-code once the closing token arrives.
 *
 * When `isStreaming` becomes false the wrapper snaps to the full content
 * and stops animating.
 *
 * # Cost model
 *
 * Every committed frame re-renders this subtree, so the sanitize + parse
 * work is paid once per commit for as long as a message streams. Two rules
 * keep that bounded:
 *
 *   - The loop is **wake-on-data**: it stops as soon as the displayed text
 *     has caught up, and the content effect restarts it when more text
 *     lands. It must never re-arm itself while idle. An idle tick is cheap
 *     on its own, but it keeps the compositor scheduling frames for as long
 *     as `isStreaming` is true, and `isStreaming` deliberately stays true
 *     across phases where no text arrives at all.
 *   - Sanitizing and markdown-parsing touch the **unstable tail** only.
 *     Text before the last blank line can no longer change shape, so it is
 *     split into memoized blocks that are neither re-sanitized nor
 *     re-parsed. The block *split* itself still scans the whole displayed
 *     string once per commit; making that incremental would be a further
 *     improvement and is not done here.
 */

const TYPEWRITER_DEFAULT_FRAME_RATE = 60; // assume RAF ~60fps
const TYPEWRITER_LINUX_FRAME_RATE = 18;   // WebKitGTK has less headroom for markdown/layout work
const TYPEWRITER_BASE_CPS = 240;          // baseline visible characters/second
const TYPEWRITER_CATCHUP_FRACTION = 0.18;       // while streaming
const TYPEWRITER_DRAIN_FRACTION   = 0.35;       // after stream ends — drain faster

const isLinuxRuntime = (): boolean => {
  if (typeof document !== 'undefined') {
    const platform = document.documentElement.getAttribute('data-platform');
    if (platform) return platform === 'linux';
  }
  return typeof navigator !== 'undefined' && /linux/i.test(navigator.userAgent);
};

const resolveTypewriterFrameRate = (): number =>
  isLinuxRuntime() ? TYPEWRITER_LINUX_FRAME_RATE : TYPEWRITER_DEFAULT_FRAME_RATE;

const noop = (): void => {};

const useTypewriterBuffer = (accumulated: string, isStreaming: boolean): string => {
  const source = accumulated || '';
  const [displayed, setDisplayed] = useState(() => (isStreaming ? '' : source));
  const accRef = useRef(source);
  const lenRef = useRef(displayed.length);
  const streamingRef = useRef(isStreaming);
  const lastCommitAtRef = useRef(0);
  // Handle of the frame currently scheduled, or null when the loop is idle.
  // Doubles as the "is the loop running?" flag that keeps `wake` from
  // scheduling a second frame on top of a live one.
  const frameRef = useRef<number | null>(null);
  // The running loop's "start if idle" entry point, published for the
  // content effect below. Reset to a noop on unmount so a late wake can't
  // resurrect the loop.
  const wakeRef = useRef<() => void>(noop);

  // Mirror `isStreaming` into a ref so the RAF callback reads the current
  // value without being re-created (see the mount-only effect below).
  // Writing `ref.current` during render trips the `react-hooks/refs` lint
  // rule, so the mirror lives in an effect keyed on the value.
  useEffect(() => {
    streamingRef.current = isStreaming;
  }, [isStreaming]);

  // The animation loop. Created once per mount: restarting it whenever the
  // content changes would cancel the pending frame on every delta, and when
  // deltas arrive faster than frames that starves the loop and nothing ever
  // renders. Instead the loop is long-lived and idles between bursts.
  useEffect(() => {
    let cancelled = false;
    // The platform cannot change while this component is mounted, so the
    // frame-rate probe (a DOM read) is resolved once per loop rather than
    // once per frame.
    const frameRate = resolveTypewriterFrameRate();
    const minFrameMs = 1000 / frameRate;
    const minAdvance = Math.max(2, Math.ceil(TYPEWRITER_BASE_CPS / frameRate));

    const tick = (now: number) => {
      frameRef.current = null;
      if (cancelled) return;
      const target = accRef.current.length;
      const cur = lenRef.current;

      if (cur < target) {
        // Behind the source: advance a paced slice, capped to the frame rate.
        if (lastCommitAtRef.current > 0 && now - lastCommitAtRef.current < minFrameMs) {
          frameRef.current = requestAnimationFrame(tick);
          return;
        }
        const lag = target - cur;
        const fraction = streamingRef.current
          ? TYPEWRITER_CATCHUP_FRACTION
          : TYPEWRITER_DRAIN_FRACTION;
        const advance = Math.min(lag, Math.max(minAdvance, Math.ceil(lag * fraction)));
        const newLen = cur + advance;
        lenRef.current = newLen;
        lastCommitAtRef.current = now;
        setDisplayed(accRef.current.slice(0, newLen));
        frameRef.current = requestAnimationFrame(tick);
        return;
      }

      // Caught up. Snap to the source and go idle *without* re-arming: the
      // content effect below restarts the loop when more text lands. The
      // snap also covers the source shrinking or being swapped for a
      // different string of the same length (a different message rendered
      // into this same component instance); React bails out of the
      // identical-string case on its own.
      lenRef.current = target;
      setDisplayed(accRef.current);
    };

    const wake = () => {
      if (cancelled || frameRef.current != null) return;
      frameRef.current = requestAnimationFrame(tick);
    };

    wakeRef.current = wake;
    wake();

    return () => {
      cancelled = true;
      wakeRef.current = noop;
      if (frameRef.current != null) cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    };
  }, []);

  // New content — or an `isStreaming` flip, which changes both the pacing
  // fraction and the post-stream drain — restarts the loop if it went idle.
  // This is also the only thing that re-arms it, so a message that stops
  // growing costs nothing until it grows again.
  useEffect(() => {
    accRef.current = source;
    wakeRef.current();
  }, [source, isStreaming]);

  return displayed;
};

/**
 * Return a "render-safe" version of partial markdown. The goal is to
 * keep the rendered DOM tree stable as new chars stream in — the same
 * block shouldn't appear, disappear, and reappear because closing
 * syntax was mid-arrival.
 *
 * Only ever applied to the **live tail** (see `splitStableMarkdownBlocks`).
 * Every heuristic here is about syntax that is incomplete *because more
 * text is still coming*, which by definition sits at the very end of the
 * stream; running them over already-stable blocks re-scans the whole
 * message on every commit for no benefit.
 *
 * Note that tail-only is not merely an optimization: applied to the whole
 * message these heuristics actively corrupt stable text.
 *
 *   - Heuristic 3 slices the string at an unmatched `[`, so one stray
 *     bracket used to drop every later block from the live view.
 *   - The fence counter below and `splitStableMarkdownBlocks` disagree about
 *     what counts as a fence. The counter matches every ``` occurrence
 *     anywhere, including inline mid-line, and ignores `~~~` entirely;
 *     `matchFence` accepts ``` or `~~~` with up to three leading spaces but
 *     only at the start of a line. So a stable paragraph merely mentioning
 *     ``` inline flipped whole-string parity odd and appended a closing
 *     fence to the *end of the message* — a spurious empty code block glued
 *     onto the live tail, while the completed blocks themselves were left
 *     alone. (The `~~~` half of that asymmetry is pre-existing and untouched
 *     here; it simply no longer sees stable text.)
 */
export const stabilizePartialMarkdown = (text: string): string => {
  if (!text) return text;
  let out = text;

  // 1. Auto-close unclosed fenced code block. Without this, `bash\nls -la`
  //    inside an unclosed fence renders as literal text for the duration of
  //    the block, then suddenly snaps into a styled <pre> when the closing
  //    fence finally arrives. Auto-closing makes the code block exist from
  //    the moment the opening fence is parsed; content then grows inside it.
  const fenceMatches = out.match(/```/g);
  const fenceCount = fenceMatches ? fenceMatches.length : 0;
  if (fenceCount % 2 === 1) {
    out = out.replace(/\s*$/, '') + '\n```';
  }

  // The remaining heuristics are unsafe inside an open code block, so only
  // apply them when fences are balanced (we're back in prose).
  if (fenceCount % 2 === 0) {
    // 2. Strip a trailing unmatched single backtick (would briefly render the
    //    rest of the line as inline code once a second backtick arrived).
    const withoutFences = out.replace(/```[\s\S]*?```/g, '');
    const tickCount = (withoutFences.match(/`/g) || []).length;
    if (tickCount % 2 === 1) {
      out = out.replace(/`([^`\n]*)$/, '$1');
    }

    // 3. If a `[` appears after the last `]`, the user is mid-link/image.
    //    Rendering it raw shows `[partial text` until `](url)` arrives,
    //    then the whole bracketed phrase suddenly becomes a link. Hide the
    //    incomplete bracket entirely; it'll reappear (whole) when complete.
    const lastClose = out.lastIndexOf(']');
    const lastOpen = out.lastIndexOf('[');
    if (lastOpen > lastClose) {
      out = out.slice(0, lastOpen);
    }
  }

  return out;
};

interface StreamingMarkdownProps {
  content: string;
  isStreaming?: boolean;
}

interface MarkdownSplit {
  completed: string[];
  tail: string;
}

interface FenceState {
  char: '`' | '~';
  length: number;
  rest: string;
}

const matchFence = (line: string): FenceState | null => {
  const match = /^( {0,3})(`{3,}|~{3,})(.*)$/.exec(line);
  if (!match) return null;
  const marker = match[2]!;
  const char = marker[0] === '`' ? '`' : '~';
  return { char, length: marker.length, rest: match[3] ?? '' };
};

export const splitStableMarkdownBlocks = (text: string): MarkdownSplit => {
  if (!text) return { completed: [], tail: '' };

  const completed: string[] = [];
  let fence: FenceState | null = null;
  let blockStart = 0;
  let lineStart = 0;

  for (let index = 0; index < text.length; index += 1) {
    if (text[index] !== '\n') continue;

    const line = text.slice(lineStart, index);
    const marker = matchFence(line);
    if (marker) {
      if (!fence) {
        fence = marker;
      } else if (
        marker.char === fence.char
        && marker.length >= fence.length
        && marker.rest.trim() === ''
      ) {
        fence = null;
      }
    }

    if (!fence && line.trim() === '') {
      const block = text.slice(blockStart, index + 1);
      if (block.trim()) {
        completed.push(block);
      }
      blockStart = index + 1;
    }

    lineStart = index + 1;
  }

  return {
    completed,
    tail: text.slice(blockStart),
  };
};

const blockKey = (block: string, index: number): string => `${index}:${block.length}`;

const StableMarkdownBlock = memo(({ content }: { content: string }) => (
  <MarkdownBlock content={content} isStreaming={false} />
));

StableMarkdownBlock.displayName = 'StableMarkdownBlock';

const StreamingMarkdownBlocks = memo(({ content, isStreaming }: { content: string; isStreaming: boolean }) => {
  const split = useMemo(() => splitStableMarkdownBlocks(content), [content]);
  // Sanitize only the live tail. `split.tail` is a short, stable-identity
  // string for as long as the tail doesn't change, so the memo also skips
  // the work entirely on re-renders driven by anything else.
  const safeTail = useMemo(() => stabilizePartialMarkdown(split.tail), [split.tail]);

  return (
    <div className={styles.markdownContainer}>
      {split.completed.map((block, index) => (
        <StableMarkdownBlock key={blockKey(block, index)} content={block} />
      ))}
      {safeTail && <MarkdownBlock content={safeTail} isStreaming={isStreaming} />}
    </div>
  );
});

StreamingMarkdownBlocks.displayName = 'StreamingMarkdownBlocks';

const StreamingMarkdown = memo(({ content, isStreaming = false }: StreamingMarkdownProps) => {
  const source = content || '';
  const displayed = useTypewriterBuffer(source, isStreaming);
  // Use the typewriter output whenever it's behind (still streaming or
  // draining post-stream). Once fully caught up, switch to the raw
  // source so we stop running the sanitizer over completed content.
  const isCatchingUp = displayed.length < source.length;
  const useBuffered = isStreaming || isCatchingUp;
  if (!useBuffered) {
    return <MarkdownMessage content={source} isStreaming={false} />;
  }
  return <StreamingMarkdownBlocks content={displayed} isStreaming={useBuffered} />;
});

StreamingMarkdown.displayName = 'StreamingMarkdown';

export default StreamingMarkdown;
