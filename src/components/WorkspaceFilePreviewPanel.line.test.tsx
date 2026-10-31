import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { CodeView } from './WorkspaceFilePreviewPanel';
import styles from './WorkspaceFilePreviewPanel.module.css';

describe('CodeView line navigation', () => {
  it('scrolls back to the same line on a new navigation', () => {
    const scrollIntoView = vi.fn();
    Element.prototype.scrollIntoView = scrollIntoView;
    const content = 'first\nsecond\nthird';
    const { rerender } = render(<CodeView content={content} language="text" line={2} navigationId={1} />);

    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    rerender(<CodeView content={content} language="text" line={2} navigationId={2} />);

    expect(scrollIntoView).toHaveBeenCalledTimes(2);
    expect(scrollIntoView).toHaveBeenLastCalledWith({ block: 'center' });
  });

  it('highlights and scrolls the requested source line', () => {
    const scrollIntoView = vi.fn();
    Element.prototype.scrollIntoView = scrollIntoView;
    render(<CodeView content={'first\nsecond\nthird'} language="text" line={2} />);

    const line = screen.getByText('second').closest('[data-preview-line]');
    expect(line).toHaveAttribute('data-preview-line', '2');
    expect(line).toHaveClass(styles.targetLine!);
    expect(scrollIntoView).toHaveBeenCalledWith({ block: 'center' });
  });
});
