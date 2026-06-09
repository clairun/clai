import React, { memo, useEffect, useRef, useState } from 'react';
import MarkdownMessage from './MarkdownMessage';

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
 */

const TYPEWRITER_FRAME_RATE = 60; // assume RAF ~60fps
const TYPEWRITER_BASE_CPS = 240;  // baseline visible characters/second
const TYPEWRITER_MIN_ADVANCE = Math.max(2, Math.ceil(TYPEWRITER_BASE_CPS / TYPEWRITER_FRAME_RATE));
const TYPEWRITER_CATCHUP_FRACTION = 0.18;       // while streaming
const TYPEWRITER_DRAIN_FRACTION   = 0.35;       // after stream ends — drain faster

const useTypewriterBuffer = (accumulated: string, isStreaming: boolean): string => {
  const [displayed, setDisplayed] = useState(() => (isStreaming ? '' : accumulated || ''));
  const accRef = useRef(accumulated || '');
  const lenRef = useRef(displayed.length);
  const streamingRef = useRef(isStreaming);

  // Mirror the changing inputs into refs so the RAF `tick` callback always
  // reads the latest values without being in the effect dep list (and
  // re-creating the loop on every change). Writing `ref.current` directly
  // during render trips the `react-hooks/refs` lint rule, so the mirror
  // lives in effects keyed on the input values.
  useEffect(() => {
    accRef.current = accumulated || '';
  }, [accumulated]);
  useEffect(() => {
    streamingRef.current = isStreaming;
  }, [isStreaming]);

  useEffect(() => {
    // If the consumer never asked for streaming, just mirror content
    // immediately. This also covers the case where the message arrives
    // already-complete (e.g., loaded from history).
    if (!isStreaming && lenRef.current >= accRef.current.length) {
      lenRef.current = accRef.current.length;
      setDisplayed(accRef.current);
      return undefined;
    }

    let cancelled = false;
    let frame: number | null = null;

    const tick = () => {
      if (cancelled) return;
      const target = accRef.current.length;
      const cur = lenRef.current;

      if (cur < target) {
        const lag = target - cur;
        const fraction = streamingRef.current
          ? TYPEWRITER_CATCHUP_FRACTION
          : TYPEWRITER_DRAIN_FRACTION;
        const advance = Math.min(
          lag,
          Math.max(TYPEWRITER_MIN_ADVANCE, Math.ceil(lag * fraction))
        );
        const newLen = cur + advance;
        lenRef.current = newLen;
        setDisplayed(accRef.current.slice(0, newLen));
        frame = requestAnimationFrame(tick);
        return;
      }

      if (cur > target) {
        // Defensive: accumulated shrank (shouldn't happen with append-only
        // streams, but loading a different message into the same component
        // could). Snap back to whatever the source says now.
        lenRef.current = target;
        setDisplayed(accRef.current);
      }

      if (streamingRef.current) {
        // Caught up — keep polling for new deltas
        frame = requestAnimationFrame(tick);
      }
      // Not streaming and caught up → stop. The next isStreaming flip or
      // content append will re-arm us via the effect dep / ref read.
    };

    frame = requestAnimationFrame(tick);

    return () => {
      cancelled = true;
      if (frame) cancelAnimationFrame(frame);
    };
  }, [isStreaming]);

  return displayed;
};

/**
 * Return a "render-safe" version of partial markdown. The goal is to
 * keep the rendered DOM tree stable as new chars stream in — the same
 * block shouldn't appear, disappear, and reappear because closing
 * syntax was mid-arrival.
 */
const stabilizePartialMarkdown = (text: string): string => {
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

const StreamingMarkdown = memo(({ content, isStreaming = false }: StreamingMarkdownProps) => {
  const source = content || '';
  const displayed = useTypewriterBuffer(source, isStreaming);
  // Use the typewriter output whenever it's behind (still streaming or
  // draining post-stream). Once fully caught up, switch to the raw
  // source so we stop running the sanitizer over completed content.
  const isCatchingUp = displayed.length < source.length;
  const useBuffered = isStreaming || isCatchingUp;
  const safe = useBuffered ? stabilizePartialMarkdown(displayed) : source;
  return <MarkdownMessage content={safe} isStreaming={useBuffered} />;
});

StreamingMarkdown.displayName = 'StreamingMarkdown';

export default StreamingMarkdown;
