import { describe, it, expect } from 'vitest';
import { computeTextareaSize } from './composerTextarea';

const LINE = 20;
const MAX = 150;

describe('computeTextareaSize', () => {
  it('stays single-line height with no scrollbar for empty/one-line content', () => {
    expect(computeTextareaSize(LINE, LINE, MAX)).toEqual({ height: LINE, overflowY: 'hidden' });
  });

  it('never reports less than one line even if scrollHeight underflows', () => {
    expect(computeTextareaSize(0, LINE, MAX)).toEqual({ height: LINE, overflowY: 'hidden' });
  });

  it('grows with content while below the cap, scrollbar still hidden', () => {
    // 4 lines worth of content, under the 150px cap.
    expect(computeTextareaSize(80, LINE, MAX)).toEqual({ height: 80, overflowY: 'hidden' });
  });

  it('caps height and enables scrolling once content exceeds the cap', () => {
    // 10 lines worth of content (200px) > 150px cap.
    expect(computeTextareaSize(200, LINE, MAX)).toEqual({ height: MAX, overflowY: 'auto' });
  });

  it('does not enable scrolling when content exactly equals the cap', () => {
    expect(computeTextareaSize(MAX, LINE, MAX)).toEqual({ height: MAX, overflowY: 'hidden' });
  });
});
