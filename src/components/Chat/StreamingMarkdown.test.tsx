import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';

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

import StreamingMarkdown, { splitStableMarkdownBlocks } from './StreamingMarkdown';

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
});
