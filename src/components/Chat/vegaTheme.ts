import type { Config } from 'vega-embed';

/**
 * Host-side Vega-Lite theme.
 *
 * Charts are styled by CLAI, not by the model: a spec carries semantics only
 * (data, marks, encodings) and gets the app's palette, typography and
 * surface colors injected at render time, so it matches the rest of the UI
 * and flips with the dark/light theme. The tokens come from the `--chart-*`
 * custom properties in `src/styles/theme-{dark,light}.css`; a spec may still
 * override anything through its own `config` (vega-embed merges spec config
 * over ours).
 */

export interface ChartThemeTokens {
  /** Categorical palette, in order. */
  colors: string[];
  /** Axis/legend title and chart title color. */
  title: string;
  /** Axis/legend label color. */
  label: string;
  /** Gridline color. */
  grid: string;
  /** Axis domain and tick color. */
  axis: string;
  /** Font family for every text mark. */
  font: string;
}

const PALETTE_SIZE = 12;

const FALLBACK_TOKENS: ChartThemeTokens = {
  colors: [
    '#6366F1', '#00AB94', '#8B5CF6', '#0891B2', '#EA580C', '#DC2626',
    '#2563EB', '#64748B', '#0D9488', '#9333EA', '#CA8A04', '#BE185D',
  ],
  title: '#1F2328',
  label: '#656D76',
  grid: 'rgba(45, 42, 38, 0.06)',
  axis: 'rgba(45, 42, 38, 0.15)',
  font: 'system-ui, sans-serif',
};

/**
 * Read the chart tokens from the live document. `readVar` resolves a custom
 * property name to its computed value (empty when undefined) so the reader
 * is injectable in tests; the default reads `<html>`'s computed style.
 */
export const readChartThemeTokens = (
  readVar: (name: string) => string = defaultReadVar,
  font: string = defaultFont(),
): ChartThemeTokens => {
  const colors: string[] = [];
  for (let i = 1; i <= PALETTE_SIZE; i += 1) {
    const value = readVar(`--chart-color-${i}`);
    if (value) colors.push(value);
  }
  return {
    colors: colors.length > 0 ? colors : FALLBACK_TOKENS.colors,
    title: readVar('--chart-title') || FALLBACK_TOKENS.title,
    label: readVar('--chart-label') || FALLBACK_TOKENS.label,
    grid: readVar('--chart-grid') || FALLBACK_TOKENS.grid,
    axis: readVar('--chart-axis') || FALLBACK_TOKENS.axis,
    font: font || FALLBACK_TOKENS.font,
  };
};

function defaultReadVar(name: string): string {
  if (typeof document === 'undefined') return '';
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function defaultFont(): string {
  if (typeof document === 'undefined' || !document.body) return '';
  return getComputedStyle(document.body).fontFamily;
}

/** Build the Vega-Lite `config` object for a set of tokens. */
export const buildVegaConfig = (tokens: ChartThemeTokens): Config => ({
  // The surrounding card supplies the surface; a transparent view lets the
  // CSS-driven background (and theme flips) show through.
  background: 'transparent',
  font: tokens.font,
  // Hover tooltips on every mark by default — the model shouldn't have to
  // ask for the single most useful interaction.
  mark: { tooltip: true },
  range: { category: tokens.colors },
  view: { stroke: null },
  title: {
    color: tokens.title,
    fontSize: 14,
    fontWeight: 600,
    anchor: 'start',
    offset: 12,
  },
  axis: {
    labelColor: tokens.label,
    titleColor: tokens.title,
    gridColor: tokens.grid,
    domainColor: tokens.axis,
    tickColor: tokens.axis,
    labelFontSize: 11,
    titleFontSize: 12,
    titleFontWeight: 500,
    titlePadding: 8,
  },
  legend: {
    labelColor: tokens.label,
    titleColor: tokens.title,
    labelFontSize: 11,
    titleFontSize: 12,
    titleFontWeight: 500,
  },
  header: {
    labelColor: tokens.label,
    titleColor: tokens.title,
  },
});
