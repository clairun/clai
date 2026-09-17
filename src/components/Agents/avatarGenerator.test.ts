import { describe, expect, it } from 'vitest';
import {
  GENERATOR_VERSION,
  HUES,
  agentAvatar,
  avatarParts,
  hueIndexFor,
  paletteForHue,
  type AvatarMood,
} from './avatarGenerator';

const SEEDS = ['Manager', 'Code Reviewer', 'e9367b47-a656-4244-9614-243fcc7e662b', '', 'ü🙂', 'x'];

// Ground with `avatarParts` so that, together, these six seeds draw every body,
// eye, mouth and marker kind at least once. If a part table changes, re-grind.
const COVERAGE_SEEDS = ['cov-0', 'cov-3', 'cov-17', 'cov-214', 'cov-2', 'cov-7'];
// visor eyes + line mouth + antenna: every part of this face reacts to mood.
const MOOD_SEED = 'mood-100';
// dots + smile: exercises the idle lid and the idle → flat mouth swap.
const LID_SEED = 'mood-4';
// wedges + none / flat: the two faces idle legitimately leaves unchanged.
const STOIC_SEEDS = ['mood-22', 'mood-31'];
const MOODS: AvatarMood[] = ['neutral', 'running', 'attention', 'idle'];

describe('agentAvatar', () => {
  it('is deterministic: same seed and options give the same SVG', () => {
    for (const seed of SEEDS) {
      expect(agentAvatar(seed)).toBe(agentAvatar(seed));
      expect(agentAvatar(seed, { theme: 'light', mood: 'running', size: 20 })).toBe(
        agentAvatar(seed, { theme: 'light', mood: 'running', size: 20 }),
      );
    }
  });

  it('pins the v1 drawing: a change to any part table or geometry must bump GENERATOR_VERSION', () => {
    // If this fails on purpose, bump GENERATOR_VERSION and keep v1 reachable.
    expect(GENERATOR_VERSION).toBe(1);
    // The coverage seeds really do cover every renderer (guards the snapshot's reach).
    const kinds = COVERAGE_SEEDS.map(avatarParts);
    expect(new Set(kinds.map((k) => k.body)).size).toBe(4);
    expect(new Set(kinds.map((k) => k.eyes)).size).toBe(6);
    expect(new Set(kinds.map((k) => k.mouth)).size).toBe(5);
    expect(new Set(kinds.map((k) => k.marker)).size).toBe(4);
    // The mood seeds must keep the parts that make their snapshots meaningful.
    expect(avatarParts(MOOD_SEED)).toMatchObject({ eyes: 'visor', mouth: 'line', marker: 'antenna' });
    expect(avatarParts(LID_SEED)).toMatchObject({ eyes: 'dots', mouth: 'smile' });
    expect(STOIC_SEEDS.map((seed) => avatarParts(seed))).toMatchObject([
      { eyes: 'wedges', mouth: 'none' },
      { eyes: 'wedges', mouth: 'flat' },
    ]);
    expect(COVERAGE_SEEDS.map((seed) => agentAvatar(seed, { size: 40 }))).toMatchSnapshot('parts');
    expect(MOODS.map((mood) => agentAvatar(MOOD_SEED, { size: 40, mood }))).toMatchSnapshot('moods');
    expect(MOODS.map((mood) => agentAvatar(LID_SEED, { size: 40, mood }))).toMatchSnapshot('lids');
    expect(HUES.map((hue) => paletteForHue(hue, 'light'))).toMatchSnapshot('palette-light');
    expect(HUES.map((hue) => paletteForHue(hue, 'dark'))).toMatchSnapshot('palette-dark');
  });

  it('never echoes the seed into the markup', () => {
    const seed = '<script>alert(1)</script>"onload="x';
    const svg = agentAvatar(seed);
    expect(svg).not.toContain('script');
    expect(svg).not.toContain('onload');
    expect(svg.startsWith('<svg xmlns="http://www.w3.org/2000/svg"')).toBe(true);
    expect(svg.endsWith('</svg>')).toBe(true);
  });

  it('honours size, theme and background', () => {
    expect(agentAvatar('a', { size: 20 })).toContain('width="20" height="20"');
    expect(agentAvatar('a', { theme: 'dark' })).not.toBe(agentAvatar('a', { theme: 'light' }));
    const withBg = agentAvatar('a');
    const noBg = agentAvatar('a', { background: false });
    expect(withBg.length).toBeGreaterThan(noBg.length);
  });

  it('gives each seed its own clipPath id so many avatars can share a document', () => {
    const ids = SEEDS.map((seed) => /clipPath id="([^"]+)"/.exec(agentAvatar(seed))?.[1]);
    expect(new Set(ids).size).toBe(SEEDS.length);
    for (const [i, seed] of SEEDS.entries()) {
      expect(agentAvatar(seed)).toContain(`clip-path="url(#${ids[i]})"`);
    }
  });

  it('reaches all 12 hues and every part kind over a few thousand seeds', () => {
    const hues = new Set<number>();
    const bodies = new Set<string>();
    const eyes = new Set<string>();
    const mouths = new Set<string>();
    const markers = new Set<string>();
    for (let i = 0; i < 3000; i++) {
      const parts = avatarParts(`seed-${i}`);
      hues.add(parts.hueIndex);
      bodies.add(parts.body);
      eyes.add(parts.eyes);
      mouths.add(parts.mouth);
      markers.add(parts.marker);
    }
    expect(hues.size).toBe(HUES.length);
    expect([...bodies].sort()).toEqual(['blob', 'pill', 'squircle', 'tall']);
    expect([...eyes].sort()).toEqual(['dots', 'odd', 'rings', 'tall', 'visor', 'wedges']);
    expect([...mouths].sort()).toEqual(['flat', 'line', 'none', 'o', 'smile']);
    expect([...markers].sort()).toEqual(['antenna', 'fin', 'none', 'tuft']);
  });

  it('spreads hues roughly evenly (no hue below half the expected share)', () => {
    const counts = new Array<number>(HUES.length).fill(0);
    const total = 2400;
    for (let i = 0; i < total; i++) counts[hueIndexFor(`agent-${i}`)]! += 1;
    const expected = total / HUES.length;
    for (const c of counts) expect(c).toBeGreaterThan(expected / 2);
  });

  it('hueIndexFor matches the hue the drawing uses', () => {
    for (const seed of SEEDS) {
      const idx = hueIndexFor(seed);
      const pal = paletteForHue(HUES[idx] as number);
      expect(agentAvatar(seed)).toContain(`fill="${pal.bg}"`);
      expect(avatarParts(seed).hueIndex).toBe(idx);
    }
  });

  it('a hue override recolours without changing the face', () => {
    const strip = (svg: string) => svg.replace(/#[0-9a-f]{6}/g, '#');
    const plain = agentAvatar('Manager');
    const indigo = agentAvatar('Manager', { hue: 277 });
    expect(indigo).not.toBe(plain);
    expect(strip(indigo)).toBe(strip(plain));
  });

  it('mood changes the expression but not the body or hue', () => {
    const bodyOf = (svg: string) => /<g clip-path[^>]*>(.*?)<ellipse/.exec(svg)?.[1];
    let unchangedByIdle = 0;
    for (const seed of [...SEEDS, ...COVERAGE_SEEDS, MOOD_SEED, LID_SEED, ...STOIC_SEEDS]) {
      const rendered = MOODS.map((mood) => agentAvatar(seed, { mood }));
      // Marker + body path prefix is identical unless the marker glows while running.
      const neutralBody = bodyOf(rendered[0]!);
      expect(bodyOf(rendered[2]!)).toBe(neutralBody);
      expect(bodyOf(rendered[3]!)).toBe(neutralBody);
      // Attention always differs: the mouth becomes an "o" and any eye kind but
      // wedges widens; wedges still get the "o".
      expect(rendered[2]).not.toBe(rendered[0]);
      // Idle drops lids on every eye kind except wedges and flattens any mouth
      // except none/flat — so wedges + none/flat is legitimately unchanged.
      const { eyes, mouth } = avatarParts(seed);
      if (eyes !== 'wedges' || (mouth !== 'none' && mouth !== 'flat')) {
        expect(rendered[3]).not.toBe(rendered[0]);
      } else {
        expect(rendered[3]).toBe(rendered[0]);
        unchangedByIdle += 1;
      }
    }
    expect(unchangedByIdle).toBe(STOIC_SEEDS.length);
  });

  it('running only adds glow/dots — it never moves the body', () => {
    const running = agentAvatar(MOOD_SEED, { mood: 'running' });
    const neutral = agentAvatar(MOOD_SEED);
    expect(running.length).toBeGreaterThan(neutral.length);
    // Same body path in both.
    const body = /<path d="M[^"]*" fill="#[0-9a-f]{6}"\/><ellipse/.exec(neutral)?.[0];
    expect(body).toBeDefined();
    expect(running).toContain(body);
  });

  it('palette lightness is theme-appropriate (dark bg on dark theme, light bg on light)', () => {
    const luminance = (hex: string) => {
      const v = parseInt(hex.slice(1), 16);
      return ((v >> 16) & 255) * 0.2126 + ((v >> 8) & 255) * 0.7152 + (v & 255) * 0.0722;
    };
    for (const hue of HUES) {
      const dark = paletteForHue(hue, 'dark');
      const light = paletteForHue(hue, 'light');
      expect(luminance(dark.bg)).toBeLessThan(90);
      expect(luminance(light.bg)).toBeGreaterThan(200);
      expect(luminance(dark.ink)).toBeLessThan(luminance(dark.body));
      expect(luminance(light.ink)).toBeLessThan(luminance(light.body));
      // On dark the ring is the body colour; on light it is a darker cut of it.
      expect(dark.ring).toBe(dark.body);
      expect(luminance(light.ring)).toBeLessThan(luminance(light.body));
    }
  });
});
