import { describe, expect, it } from 'vitest';
import { ACCESSORIES, SCREENTONES, WORKSPACE_HUES, workspaceLook } from './workspaceLook';

describe('workspaceLook', () => {
  it('is the same for the same id, run after run', () => {
    expect(workspaceLook('0fb97776-4239-4dd7-812d-3b5e1328e524')).toEqual(workspaceLook('0fb97776-4239-4dd7-812d-3b5e1328e524'));
    expect(workspaceLook('ws-a')).toMatchInlineSnapshot(`
      {
        "accessory": "headband",
        "hue": 110,
        "screentone": "lines",
      }
    `);
  });

  it('spreads ids over every accessory, hue and screentone, none far from its share', () => {
    const looks = Array.from({ length: 800 }, (_, i) => workspaceLook(`00000000-0000-4000-8000-${String(i).padStart(12, '0')}`));
    const shares = (values: readonly unknown[], trait: keyof (typeof looks)[number]) =>
      values.map((v) => (looks.filter((l) => l[trait] === v).length * values.length) / looks.length);
    for (const share of [...shares(ACCESSORIES, 'accessory'), ...shares(WORKSPACE_HUES, 'hue'), ...shares(SCREENTONES, 'screentone')]) {
      expect(share).toBeGreaterThan(0.7);
      expect(share).toBeLessThan(1.3);
    }
  });

  it('gives neighbouring ids different looks', () => {
    const looks = new Set(['ws-1', 'ws-2', 'ws-3', 'ws-4', 'ws-5', 'ws-6'].map((id) => JSON.stringify(workspaceLook(id))));
    expect(looks.size).toBe(6);
  });
});
