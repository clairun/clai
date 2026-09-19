/**
 * CLAI's procedural agent avatar.
 *
 * Every agent gets a face drawn from a seed string: a body (blob, squircle,
 * pill or tall capsule), a pair of eyes, a mouth and an optional marker
 * (antenna, fin, tuft) in one of twelve OKLCH hues. Same seed → same SVG,
 * byte for byte, on every platform; no network, no dependency, no font.
 *
 * `mood` is a render-time input, not part of the identity: the same face
 * looks alert while its agent runs, wide-eyed when it needs the user, and
 * half-asleep when idle or disabled.
 *
 * Capacity: 12 hues × 4 bodies × 6 eyes × 5 mouths × 4 markers = 5 760
 * recognisably different faces, each with continuous variation (blob wobble,
 * eye spacing, antenna tilt). The seed is hashed to 32 bits, so the hard
 * ceiling on byte-distinct outputs is 2³².
 *
 * `GENERATOR_VERSION` is stored next to every saved seed. Changing any part
 * table, weight or geometry below changes faces for existing seeds — the
 * snapshot test will tell you. Only one version exists today, so `version` is
 * recorded but not yet dispatched on; when a redraw is wanted, bump the
 * constant and branch on `options.version` in `agentAvatar` so saved seeds
 * keep their v1 look.
 */

export type AvatarTheme = 'light' | 'dark';
export type AvatarMood = 'neutral' | 'running' | 'attention' | 'idle';

export const GENERATOR_VERSION = 1;

/** Twelve hues (OKLCH degrees) spaced so neighbours stay distinguishable on both themes. */
export const HUES: readonly number[] = [20, 50, 80, 110, 140, 170, 200, 230, 260, 290, 320, 350];

export type BodyKind = 'blob' | 'squircle' | 'pill' | 'tall';
export type EyeKind = 'dots' | 'rings' | 'visor' | 'wedges' | 'odd' | 'tall';
export type MouthKind = 'none' | 'line' | 'smile' | 'o' | 'flat';
export type MarkerKind = 'antenna' | 'fin' | 'tuft' | 'none';

// Repeated entries are weights: blobs and antennae are the "house look".
const BODIES: readonly BodyKind[] = ['blob', 'blob', 'squircle', 'pill', 'tall'];
const EYES: readonly EyeKind[] = ['dots', 'rings', 'visor', 'wedges', 'odd', 'tall'];
const MOUTHS: readonly MouthKind[] = ['none', 'line', 'smile', 'o', 'flat'];
const MARKERS: readonly MarkerKind[] = ['antenna', 'antenna', 'fin', 'tuft', 'none', 'none'];

export interface AvatarParts {
  hueIndex: number;
  body: BodyKind;
  eyes: EyeKind;
  mouth: MouthKind;
  marker: MarkerKind;
}

export interface AvatarOptions {
  /** Rendered width/height in CSS px. The viewBox is always 100×100. */
  size?: number;
  theme?: AvatarTheme;
  mood?: AvatarMood;
  /** Draw the tinted disc behind the body. Off for places that supply their own. */
  background?: boolean;
  /** Override the seed-derived hue (OKLCH degrees). Used for the Main's fixed indigo. */
  hue?: number;
  /** Saved generator version. Recorded for a future v2; every version renders as v1 today. */
  version?: number;
}

export interface AvatarPalette {
  body: string;
  bodyDeep: string;
  bg: string;
  ink: string;
  ring: string;
}

// ---- seeded randomness -------------------------------------------------------

/** MurmurHash3-style string mixer; each call yields the next 32-bit word. */
const xmur3 = (str: string): (() => number) => {
  let h = 1779033703 ^ str.length;
  for (let i = 0; i < str.length; i++) {
    h = Math.imul(h ^ str.charCodeAt(i), 3432918353);
    h = (h << 13) | (h >>> 19);
  }
  return () => {
    h = Math.imul(h ^ (h >>> 16), 2246822507);
    h = Math.imul(h ^ (h >>> 13), 3266489909);
    return (h ^= h >>> 16) >>> 0;
  };
};

/** mulberry32: small, fast, good enough for picking eyes. Returns [0, 1). */
const mulberry32 = (state: number): (() => number) => {
  let a = state;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
};

interface SeededRng {
  hash: number;
  hueIndex: number;
  rand: () => number;
  pick: <T>(arr: readonly T[]) => T;
  range: (min: number, max: number) => number;
}

const rngFor = (seed: string): SeededRng => {
  const next = xmur3(seed);
  const hash = next();
  const hueIndex = hash % HUES.length;
  const rand = mulberry32(next());
  return {
    hash,
    hueIndex,
    rand,
    // `arr` is never empty here (all tables are constants), so the index is in range.
    pick: (arr) => arr[Math.floor(rand() * arr.length)] as (typeof arr)[number],
    range: (min, max) => min + rand() * (max - min),
  };
};

/** The hue index (0–11) a seed maps to. Same function the drawing uses. */
export const hueIndexFor = (seed: string): number => rngFor(seed).hueIndex;

// ---- colour -------------------------------------------------------------------

const oklchToHex = (L: number, C: number, hueDeg: number): string => {
  const h = (hueDeg * Math.PI) / 180;
  const a = C * Math.cos(h);
  const b = C * Math.sin(h);
  const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = L - 0.0894841775 * a - 1.291485548 * b;
  const l = l_ ** 3;
  const m = m_ ** 3;
  const s = s_ ** 3;
  const r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
  const g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
  const bb = -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s;
  const gamma = (c: number): number => {
    const v = Math.min(1, Math.max(0, c));
    return v <= 0.0031308 ? 12.92 * v : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
  };
  const channel = (c: number): string =>
    Math.round(gamma(c) * 255)
      .toString(16)
      .padStart(2, '0');
  return `#${channel(r)}${channel(g)}${channel(bb)}`;
};

/** Palette for a hue (OKLCH degrees) on a theme: constant lightness/chroma, only hue varies. */
export const paletteForHue = (hue: number, theme: AvatarTheme = 'dark'): AvatarPalette =>
  theme === 'dark'
    ? {
        body: oklchToHex(0.74, 0.13, hue),
        bodyDeep: oklchToHex(0.6, 0.13, hue),
        bg: oklchToHex(0.28, 0.06, hue),
        ink: oklchToHex(0.18, 0.04, hue),
        ring: oklchToHex(0.74, 0.13, hue),
      }
    : {
        body: oklchToHex(0.7, 0.13, hue),
        bodyDeep: oklchToHex(0.56, 0.13, hue),
        bg: oklchToHex(0.93, 0.03, hue),
        ink: oklchToHex(0.22, 0.05, hue),
        ring: oklchToHex(0.6, 0.13, hue),
      };

// ---- geometry -----------------------------------------------------------------

/** Compact number formatting so the SVG stays small and snapshot-stable. */
const n = (v: number): string => {
  const s = v.toFixed(1);
  return s.endsWith('.0') ? s.slice(0, -2) : s;
};

type Pt = [number, number];

/** Closed wobbly blob: 8 points around a circle, Catmull-Rom → cubic Béziers. */
const blobPath = (cx: number, cy: number, r: number, rand: () => number): string => {
  const points = 8;
  const wobble = 0.14;
  const pts: Pt[] = [];
  for (let i = 0; i < points; i++) {
    const a = (i / points) * Math.PI * 2 - Math.PI / 2;
    const rr = r * (1 + (rand() * 2 - 1) * wobble);
    pts.push([cx + Math.cos(a) * rr, cy + Math.sin(a) * rr]);
  }
  const at = (i: number): Pt => pts[(i + points) % points] as Pt;
  let d = `M${n(at(0)[0])} ${n(at(0)[1])}`;
  for (let i = 0; i < points; i++) {
    const p0 = at(i - 1);
    const p1 = at(i);
    const p2 = at(i + 1);
    const p3 = at(i + 2);
    const c1: Pt = [p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6];
    const c2: Pt = [p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6];
    d += ` C${n(c1[0])} ${n(c1[1])} ${n(c2[0])} ${n(c2[1])} ${n(p2[0])} ${n(p2[1])}`;
  }
  return `${d}Z`;
};

/** Rounded square. k = 0.552 is a circle; higher is squarer. */
const squirclePath = (cx: number, cy: number, r: number, k: number): string => {
  const c = r * k;
  return (
    `M${n(cx)} ${n(cy - r)} C${n(cx + c)} ${n(cy - r)} ${n(cx + r)} ${n(cy - c)} ${n(cx + r)} ${n(cy)}` +
    ` C${n(cx + r)} ${n(cy + c)} ${n(cx + c)} ${n(cy + r)} ${n(cx)} ${n(cy + r)}` +
    ` C${n(cx - c)} ${n(cy + r)} ${n(cx - r)} ${n(cy + c)} ${n(cx - r)} ${n(cy)}` +
    ` C${n(cx - r)} ${n(cy - c)} ${n(cx - c)} ${n(cy - r)} ${n(cx)} ${n(cy - r)}Z`
  );
};

// ---- parts --------------------------------------------------------------------

const circle = (cx: number, cy: number, r: number, attrs: string): string =>
  `<circle cx="${n(cx)}" cy="${n(cy)}" r="${n(r)}" ${attrs}/>`;

const rect = (x: number, y: number, w: number, h: number, rx: number, fill: string): string =>
  `<rect x="${n(x)}" y="${n(y)}" width="${n(w)}" height="${n(h)}" rx="${n(rx)}" fill="${fill}"/>`;

const stroke = (color: string, width: number): string =>
  `fill="none" stroke="${color}" stroke-width="${n(width)}" stroke-linecap="round"`;

interface FaceCtx {
  ink: string;
  body: string;
  mood: AvatarMood;
}

const drawEyes = (
  kind: EyeKind,
  cx: number,
  cy: number,
  spacing: number,
  r: number,
  { ink, body, mood }: FaceCtx,
): string => {
  const lx = cx - spacing;
  const rx = cx + spacing;
  // Idle: a lid in body colour covers the upper half of each eye.
  const lid = (x: number): string =>
    mood === 'idle' ? rect(x - r * 1.2, cy - r * 1.2, r * 2.4, r * 1.1, 0, body) : '';
  const wide = mood === 'attention' ? 1.35 : 1;
  switch (kind) {
    case 'dots':
      return (
        circle(lx, cy, r * wide, `fill="${ink}"`) +
        circle(rx, cy, r * wide, `fill="${ink}"`) +
        lid(lx) +
        lid(rx)
      );
    case 'rings':
      return (
        circle(lx, cy, r * 1.3 * wide, stroke(ink, r * 0.7)) +
        circle(rx, cy, r * 1.3 * wide, stroke(ink, r * 0.7)) +
        lid(lx) +
        lid(rx)
      );
    case 'visor': {
      const w = spacing * 2 + r * 3;
      const h = r * 1.8 * wide;
      const x = cx - w / 2;
      const y = cy - h / 2;
      return (
        rect(x, y, w, h, h / 2, ink) +
        (mood === 'running' ? circle(x + h / 2 + 2, cy, h * 0.28, `fill="${body}"`) : '') +
        (mood === 'idle' ? rect(x, y, w, h / 2, 0, body) : '')
      );
    }
    case 'wedges': {
      const wedge = (x: number): string =>
        `<path d="M${n(x - r * 1.4)} ${n(cy + r * 0.6)} A${n(r * 1.4)} ${n(r * 1.4)} 0 0 1 ${n(x + r * 1.4)} ${n(cy + r * 0.6)}Z" fill="${ink}"/>`;
      return wedge(lx) + wedge(rx);
    }
    case 'odd':
      return (
        circle(lx, cy, r * wide, `fill="${ink}"`) +
        circle(rx, cy, r * 1.5 * wide, stroke(ink, r * 0.7)) +
        lid(lx) +
        lid(rx)
      );
    case 'tall': {
      const h = r * 3.6 * wide;
      return (
        rect(lx - r * 0.6, cy - h / 2, r * 1.2, h, r * 0.6, ink) +
        rect(rx - r * 0.6, cy - h / 2, r * 1.2, h, r * 0.6, ink) +
        lid(lx) +
        lid(rx)
      );
    }
  }
};

const drawMouth = (kind: MouthKind, cx: number, cy: number, w: number, { ink, mood }: FaceCtx): string => {
  if (mood === 'attention') return circle(cx, cy, w * 0.35, `fill="${ink}"`);
  const k: MouthKind = mood === 'idle' && kind !== 'none' ? 'flat' : kind;
  switch (k) {
    case 'none':
      return '';
    case 'line':
      return `<path d="M${n(cx - w / 2)} ${n(cy)} L${n(cx + w / 2)} ${n(cy)}" ${stroke(ink, w * 0.22)}/>`;
    case 'flat':
      return `<path d="M${n(cx - w / 3)} ${n(cy)} L${n(cx + w / 3)} ${n(cy)}" ${stroke(ink, w * 0.2)}/>`;
    case 'smile':
      return `<path d="M${n(cx - w / 2)} ${n(cy - w * 0.15)} Q${n(cx)} ${n(cy + w * 0.45)} ${n(cx + w / 2)} ${n(cy - w * 0.15)}" ${stroke(ink, w * 0.22)}/>`;
    case 'o':
      return circle(cx, cy, w * 0.22, `fill="${ink}"`);
  }
};

const drawMarker = (
  kind: MarkerKind,
  cx: number,
  top: number,
  tilt: number,
  { body, mood }: FaceCtx,
): string => {
  switch (kind) {
    case 'antenna': {
      const glow =
        mood === 'running' ? circle(cx + tilt, top - 14, 7, `fill="${body}" opacity="0.35"`) : '';
      return (
        glow +
        `<path d="M${n(cx)} ${n(top + 2)} L${n(cx + tilt)} ${n(top - 12)}" ${stroke(body, 4)}/>` +
        circle(cx + tilt, top - 14, 4, `fill="${body}"`)
      );
    }
    case 'fin':
      return `<path d="M${n(cx - 4)} ${n(top + 6)} L${n(cx + 6)} ${n(top - 12)} L${n(cx + 14)} ${n(top + 4)}Z" fill="${body}"/>`;
    case 'tuft':
      return `<path d="M${n(cx - 8)} ${n(top + 6)} Q${n(cx - 6)} ${n(top - 10)} ${n(cx + 2)} ${n(top - 6)} M${n(cx)} ${n(top + 4)} Q${n(cx + 6)} ${n(top - 12)} ${n(cx + 12)} ${n(top - 2)}" ${stroke(body, 4)}/>`;
    case 'none':
      return '';
  }
};

// ---- public API ---------------------------------------------------------------

/** The discrete parts a seed resolves to. Cheap; used by tests and the candidate picker. */
export const avatarParts = (seed: string): AvatarParts => {
  const { hueIndex, pick } = rngFor(seed);
  return {
    hueIndex,
    body: pick(BODIES),
    eyes: pick(EYES),
    mouth: pick(MOUTHS),
    marker: pick(MARKERS),
  };
};

/**
 * Render the avatar for `seed` as an SVG string.
 *
 * The output contains only numbers and constant markup — the seed itself is
 * hashed, never echoed — so it is safe to inject as HTML.
 */
export const agentAvatar = (seed: string, options: AvatarOptions = {}): string => {
  const { size = 64, theme = 'dark', mood = 'neutral', background = true, hue } = options;
  // Only one version exists. When v2 lands, dispatch on `options.version` here.
  const rng = rngFor(seed);
  const { rand, pick, range } = rng;
  const pal = paletteForHue(hue ?? (HUES[rng.hueIndex] as number), theme);
  const ctx: FaceCtx = { ink: pal.ink, body: pal.body, mood };

  // Draw order == rand() consumption order. Do not reorder these calls: it changes faces.
  const bodyKind = pick(BODIES);
  const eyeKind = pick(EYES);
  const mouthKind = pick(MOUTHS);
  const markerKind = pick(MARKERS);

  const cx = 50;
  const cy = 54;
  let bodyEl: string;
  let top: number;
  let halfWidth = 34;
  switch (bodyKind) {
    case 'blob':
      bodyEl = `<path d="${blobPath(cx, cy, 33, rand)}" fill="${pal.body}"/>`;
      top = 21;
      break;
    case 'squircle':
      bodyEl = `<path d="${squirclePath(cx, cy, 32, range(0.75, 1))}" fill="${pal.body}"/>`;
      top = 22;
      break;
    case 'pill':
      halfWidth = 40;
      bodyEl = rect(cx - 40, cy - 30, 80, 60, 30, pal.body);
      top = cy - 30;
      break;
    case 'tall':
      halfWidth = 28;
      bodyEl = rect(cx - 28, cy - 36, 56, 72, 28, pal.body);
      top = cy - 36;
      break;
  }
  // A soft lower shade gives volume without a gradient.
  const shade = `<ellipse cx="${n(cx)}" cy="${n(cy + 22)}" rx="${n(halfWidth * 0.9)}" ry="10" fill="${pal.bodyDeep}" opacity="0.45"/>`;

  const eyeY = cy - range(2, 8);
  const spacing = range(9, 14);
  const eyeR = range(3, 4.5);
  const mouthY = eyeY + range(14, 20);
  const markerX = cx + range(-8, 8);
  const tilt = markerKind === 'antenna' ? (rand() * 2 - 1) * 10 : 0;

  const inner =
    drawMarker(markerKind, markerX, top, tilt, ctx) +
    bodyEl +
    shade +
    drawEyes(eyeKind, cx, eyeY, spacing, eyeR, ctx) +
    drawMouth(mouthKind, cx, mouthY, 14, ctx);

  // Per-seed clip id so several avatars on one page don't share a <clipPath>.
  const clipId = `ac${rng.hash.toString(36)}`;
  const bg = background ? circle(50, 50, 50, `fill="${pal.bg}"`) : '';
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${n(size)}" height="${n(size)}" viewBox="0 0 100 100" role="img" aria-hidden="true" focusable="false">` +
    `<defs><clipPath id="${clipId}">${circle(50, 50, 50, '')}</clipPath></defs>` +
    bg +
    `<g clip-path="url(#${clipId})">${inner}</g>` +
    `</svg>`
  );
};
