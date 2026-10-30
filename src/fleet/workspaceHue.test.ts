import { describe, expect, it } from 'vitest';
import { WORKSPACE_HUES, workspaceHue } from './workspaceHue';

describe('workspaceHue', () => {
  it('is the same for the same id, run after run', () => {
    expect(workspaceHue('0fb97776-4239-4dd7-812d-3b5e1328e524')).toBe(workspaceHue('0fb97776-4239-4dd7-812d-3b5e1328e524'));
    expect(workspaceHue('ws-a')).toBe(110);
  });

  it('spreads ids over every hue, none far from its share', () => {
    const hues = Array.from({ length: 800 }, (_, i) => workspaceHue(`00000000-0000-4000-8000-${String(i).padStart(12, '0')}`));
    for (const hue of WORKSPACE_HUES) {
      const share = (hues.filter((h) => h === hue).length * WORKSPACE_HUES.length) / hues.length;
      expect(share).toBeGreaterThan(0.7);
      expect(share).toBeLessThan(1.3);
    }
  });
});
