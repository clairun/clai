import { describe, expect, it } from 'vitest';
import { WORKSPACE_HUES, workspaceLook } from './workspaceLook';

describe('workspaceLook', () => {
  it('is the same for the same id, run after run', () => {
    expect(workspaceLook('0fb97776-4239-4dd7-812d-3b5e1328e524')).toEqual(workspaceLook('0fb97776-4239-4dd7-812d-3b5e1328e524'));
    expect(workspaceLook('ws-a')).toEqual({ hue: 110 });
  });

  it('spreads ids over every hue, none far from its share', () => {
    const looks = Array.from({ length: 800 }, (_, i) => workspaceLook(`00000000-0000-4000-8000-${String(i).padStart(12, '0')}`));
    for (const hue of WORKSPACE_HUES) {
      const share = (looks.filter((l) => l.hue === hue).length * WORKSPACE_HUES.length) / looks.length;
      expect(share).toBeGreaterThan(0.7);
      expect(share).toBeLessThan(1.3);
    }
  });
});
