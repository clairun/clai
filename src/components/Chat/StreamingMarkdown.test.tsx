import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor } from '@testing-library/react';

const markdownMocks = vi.hoisted(() => ({
  markdownMessageMock: vi.fn(),
  markdownBlockMock: vi.fn(),
}));

vi.mock('./MarkdownMessage', async () => {
  const React = await import('react');
  return {
    default: (props: { content: string; isStreaming?: boolean }) => {
      markdownMocks.markdownMessageMock(props);
      return React.createElement('div', { 'data-testid': 'full-markdown' }, props.content);
    },
    MarkdownBlock: (props: { content: string; isStreaming?: boolean }) => {
      markdownMocks.markdownBlockMock(props);
      return React.createElement('div', { 'data-testid': 'markdown-block' }, props.content);
    },
  };
});

import StreamingMarkdown, {
  splitStableMarkdownBlocks,
  stabilizePartialMarkdown,
} from './StreamingMarkdown';

const waitForRenderedText = async (text: string) => {
  await waitFor(() => {
    expect(
      screen.queryAllByText((_, node) => node?.textContent?.includes(text) ?? false).length
    ).toBeGreaterThan(0);
  });
};

describe('splitStableMarkdownBlocks', () => {
  it('splits completed markdown before the live tail', () => {
    expect(splitStableMarkdownBlocks('First paragraph\n\nSecond paragraph')).toEqual({
      completed: ['First paragraph\n\n'],
      tail: 'Second paragraph',
    });
  });

  it('does not split blank lines inside fenced code blocks', () => {
    const input = '```ts\nconst a = 1;\n\nconst b = 2;\n```\n\nAfter code';
    expect(splitStableMarkdownBlocks(input)).toEqual({
      completed: ['```ts\nconst a = 1;\n\nconst b = 2;\n```\n\n'],
      tail: 'After code',
    });
  });

  it('does not treat fence-prefixed code text as a closing fence', () => {
    const input = '```text\n```not a close\n\nstill code\n```\n\nAfter code';
    expect(splitStableMarkdownBlocks(input)).toEqual({
      completed: ['```text\n```not a close\n\nstill code\n```\n\n'],
      tail: 'After code',
    });
  });

  it('keeps an unclosed fenced code block in the live tail', () => {
    const input = 'Intro\n\n```ts\nconst a = 1;\n\nconst b = 2;';
    expect(splitStableMarkdownBlocks(input)).toEqual({
      completed: ['Intro\n\n'],
      tail: '```ts\nconst a = 1;\n\nconst b = 2;',
    });
  });
});

describe('stabilizePartialMarkdown', () => {
  it('auto-closes an unclosed fence so the code block exists while it grows', () => {
    expect(stabilizePartialMarkdown('```ts\nconst a = 1;')).toBe('```ts\nconst a = 1;\n```');
  });

  it('drops a trailing unmatched inline backtick', () => {
    expect(stabilizePartialMarkdown('call `foo')).toBe('call foo');
  });

  it('hides an incomplete link bracket until the closing syntax arrives', () => {
    expect(stabilizePartialMarkdown('see [partial')).toBe('see ');
  });

  it('leaves balanced prose untouched', () => {
    expect(stabilizePartialMarkdown('plain [text](url) and `code`')).toBe(
      'plain [text](url) and `code`'
    );
  });
});

describe('StreamingMarkdown', () => {
  beforeEach(() => {
    markdownMocks.markdownMessageMock.mockClear();
    markdownMocks.markdownBlockMock.mockClear();
    document.documentElement.removeAttribute('data-platform');
  });

  it('renders completed content through the normal full markdown path', () => {
    render(<StreamingMarkdown content={'First paragraph\n\nSecond paragraph'} isStreaming={false} />);

    expect(markdownMocks.markdownMessageMock).toHaveBeenCalledWith({
      content: 'First paragraph\n\nSecond paragraph',
      isStreaming: false,
    });
    expect(markdownMocks.markdownBlockMock).not.toHaveBeenCalled();
  });

  it('does not re-render completed streaming blocks when only the live tail grows', async () => {
    const { rerender } = render(
      <StreamingMarkdown content={'First paragraph\n\nLive tail'} isStreaming />
    );

    await waitForRenderedText('First paragraph');
    await waitForRenderedText('Live tail');

    markdownMocks.markdownBlockMock.mockClear();

    rerender(<StreamingMarkdown content={'First paragraph\n\nLive tail grows'} isStreaming />);

    await waitForRenderedText('Live tail grows');

    expect(
      markdownMocks.markdownBlockMock.mock.calls.some(
        ([props]) => props.content === 'First paragraph\n\n'
      )
    ).toBe(false);
    expect(
      markdownMocks.markdownBlockMock.mock.calls.some(
        ([props]) => typeof props.content === 'string' && props.content.includes('Live tail grows')
      )
    ).toBe(true);
  });

  // The sanitizer only ever sees the live tail. Applying it to the whole
  // message (the previous behavior) let a single unmatched `[` anywhere in
  // the transcript truncate everything after it for as long as the message
  // streamed, because heuristic 3 slices the string at that bracket.
  it('sanitizes only the live tail, so an earlier unmatched bracket cannot truncate the message', async () => {
    render(<StreamingMarkdown content={'a [b\n\ntail'} isStreaming />);

    await waitForRenderedText('tail');

    const rendered = markdownMocks.markdownBlockMock.mock.calls.map(([props]) => props.content);
    expect(rendered).toContain('a [b\n\n');
    expect(rendered).toContain('tail');
  });

  describe('typewriter animation loop', () => {
    let frames: { id: number; cb: FrameRequestCallback }[] = [];
    let nextFrameId = 1;
    let nowMs = 0;

    beforeEach(() => {
      frames = [];
      nextFrameId = 1;
      nowMs = 0;
      vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
        const id = nextFrameId;
        nextFrameId += 1;
        frames.push({ id, cb });
        return id;
      });
      vi.stubGlobal('cancelAnimationFrame', (id: number) => {
        frames = frames.filter((frame) => frame.id !== id);
      });
    });

    afterEach(() => {
      vi.unstubAllGlobals();
    });

    // Defaults to advancing well past any frame-rate cap so pacing never
    // suppresses a commit. Pass a small `advanceMs` to exercise the cap.
    const runFrame = async (advanceMs = 200) => {
      nowMs += advanceMs;
      const pending = frames;
      frames = [];
      await act(async () => {
        pending.forEach((frame) => frame.cb(nowMs));
      });
    };

    const runUntilIdle = async (maxFrames = 200) => {
      let count = 0;
      while (frames.length > 0 && count < maxFrames) {
        await runFrame();
        count += 1;
      }
      return count;
    };

    // The regression that made a streaming run burn CPU for its whole
    // duration: the loop used to re-arm itself unconditionally while
    // `isStreaming` was true, so it kept ticking at the display refresh rate
    // even with nothing to draw. A run spends most of its wall time running
    // tools with no new text, so that was a permanent spin.
    it('stops scheduling frames once the displayed text has caught up', async () => {
      render(<StreamingMarkdown content={'hello world'} isStreaming />);

      const framesUsed = await runUntilIdle();

      expect(framesUsed).toBeLessThan(200);
      expect(frames).toHaveLength(0);
      await waitForRenderedText('hello world');
    });

    it('wakes the idle loop when more text arrives', async () => {
      const { rerender } = render(<StreamingMarkdown content={'abc'} isStreaming />);

      await runUntilIdle();
      expect(frames).toHaveLength(0);

      rerender(<StreamingMarkdown content={'abcdef'} isStreaming />);

      expect(frames.length).toBeGreaterThan(0);
      await runUntilIdle();
      await waitForRenderedText('abcdef');
    });

    // Same wake path, but with `isStreaming` false the whole time. The old
    // effect only re-ran on an `isStreaming` change, so a non-streaming
    // segment whose content grew (a mid-turn `assistant_message_updated`
    // swapping in longer content) stayed visually truncated forever.
    it('renders appended content that arrives while not streaming', async () => {
      const { rerender } = render(<StreamingMarkdown content={'abc'} isStreaming={false} />);

      await runUntilIdle();

      rerender(<StreamingMarkdown content={'abcdef'} isStreaming={false} />);

      await runUntilIdle();
      await waitForRenderedText('abcdef');
    });

    it('cancels its pending frame on unmount', async () => {
      const { unmount } = render(<StreamingMarkdown content={'streaming text'} isStreaming />);

      expect(frames.length).toBeGreaterThan(0);
      unmount();

      expect(frames).toHaveLength(0);
    });

    // The frame-rate cap is the only branch that re-arms WITHOUT committing.
    // If it ever stopped re-arming, the typewriter would freeze mid-message
    // with text still pending and nothing left to wake it — a silent hang.
    // The other tests advance 200ms per frame and so never enter it.
    it('holds the frame open without committing when a frame lands inside the frame-rate cap', async () => {
      // Long enough that one paced advance cannot finish it even if the
      // pacing constants change: a commit advances at most
      // max(ceil(BASE_CPS / frameRate), ceil(lag * CATCHUP_FRACTION)) chars.
      const full = 'the quick brown fox jumps over the lazy dog. '.repeat(5);
      render(<StreamingMarkdown content={full} isStreaming />);

      // The first tick commits unconditionally (`lastCommitAt` is still 0).
      await runFrame();
      const committed = screen.getByTestId('markdown-block').textContent ?? '';
      expect(committed.length).toBeGreaterThan(0);
      expect(committed.length).toBeLessThan(full.length);

      // A frame inside the cap must change nothing, but must stay armed.
      await runFrame(1);
      expect(screen.getByTestId('markdown-block').textContent).toBe(committed);
      expect(frames).toHaveLength(1);

      // Once the cap elapses the loop makes progress again.
      await runFrame();
      expect(screen.getByTestId('markdown-block').textContent!.length).toBeGreaterThan(
        committed.length
      );
    });

    // The app renders under StrictMode (src/main.tsx), which mounts, tears
    // down and re-mounts every effect. The loop lives in a mount-only effect
    // and publishes its wake handle through a ref, so that teardown/re-mount
    // must not leak a frame, strand the loop, or leave `wakeRef` on the noop
    // installed by the first cleanup.
    it('still idles and renders under StrictMode double-mounting', async () => {
      render(
        <StrictMode>
          <StreamingMarkdown content={'hello world'} isStreaming />
        </StrictMode>
      );

      await runUntilIdle();

      expect(frames).toHaveLength(0);
      await waitForRenderedText('hello world');
    });
  });
});
