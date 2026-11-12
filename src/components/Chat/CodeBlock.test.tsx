import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import MarkdownMessage from './MarkdownMessage';

const fence = (lang: string, body: string) => `\`\`\`${lang}\n${body}\n\`\`\`\n`;

describe('MarkdownMessage — code block copy button', () => {
  let writeText: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  const clickCopy = async (index = 0) => {
    await act(async () => {
      fireEvent.click(screen.getAllByRole('button', { name: 'Copy code' })[index] as HTMLElement);
    });
  };

  it('copies the exact raw source of a multi-line block with special characters', async () => {
    const body = [
      '  const s = `a ${b}` + "<tag>" & \'q\';',
      '\tindented\\n literal',
      '',
      '  émoji 🚀 → done  ',
    ].join('\n');
    render(<MarkdownMessage content={`Intro\n\n${fence('ts', body)}`} />);
    await clickCopy();
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith(body);
  });

  it('copies a block without a language label', async () => {
    render(<MarkdownMessage content={fence('', 'echo hi')} />);
    await clickCopy();
    expect(writeText).toHaveBeenCalledWith('echo hi');
  });

  it('gives each block its own button and content', async () => {
    render(<MarkdownMessage content={`${fence('sh', 'first')}\n${fence('py', 'second')}`} />);
    expect(screen.getAllByRole('button', { name: 'Copy code' })).toHaveLength(2);
    await clickCopy(1);
    expect(writeText).toHaveBeenCalledWith('second');
  });

  it('does not add a button to inline code', () => {
    render(<MarkdownMessage content="Run `npm test` now" />);
    expect(screen.getByText('npm test').tagName).toBe('CODE');
    expect(screen.queryByRole('button', { name: 'Copy code' })).not.toBeInTheDocument();
  });

  it('shows "Copied" feedback, then reverts', async () => {
    vi.useFakeTimers();
    render(<MarkdownMessage content={fence('js', 'x()')} />);
    const button = screen.getByRole('button', { name: 'Copy code' });
    await clickCopy();
    expect(button).toHaveAttribute('data-state', 'copied');
    expect(screen.getByRole('status')).toHaveTextContent('Copied');
    act(() => {
      vi.advanceTimersByTime(1200);
    });
    expect(button).toHaveAttribute('data-state', 'idle');
    expect(screen.getByRole('status')).toHaveTextContent('');
  });

  it('reports a failed copy', async () => {
    writeText.mockRejectedValue(new Error('denied'));
    render(<MarkdownMessage content={fence('js', 'x()')} />);
    await clickCopy();
    expect(screen.getByRole('button', { name: 'Copy code' })).toHaveAttribute('data-state', 'failed');
    expect(screen.getByRole('status')).toHaveTextContent('Copy failed');
  });
});

describe('MarkdownMessage — code block palette', () => {
  afterEach(() => {
    document.documentElement.removeAttribute('data-theme');
  });

  const commentColor = () => (screen.getByText('// note').closest('span') as HTMLElement).style.color;

  it('draws dark-theme code with the dark palette and follows a theme switch', async () => {
    document.documentElement.setAttribute('data-theme', 'dark');
    render(<MarkdownMessage content={fence('js', 'run(); // note')} />);
    expect(commentColor()).toBe('rgb(139, 145, 156)');
    act(() => document.documentElement.setAttribute('data-theme', 'light'));
    await waitFor(() => expect(commentColor()).toBe('rgb(100, 103, 112)'));
  });

  it('leaves the block surface to the stylesheet', () => {
    document.documentElement.setAttribute('data-theme', 'dark');
    render(<MarkdownMessage content={fence('js', 'run(); // note')} />);
    const surface = screen.getByRole('button', { name: 'Copy code' }).previousElementSibling as HTMLElement;
    expect(surface.style.background).toBe('');
    expect(surface.style.backgroundColor).toBe('');
    expect(surface.style.border).toBe('');
  });
});
