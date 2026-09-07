import { describe, expect, it } from 'vitest';
import { buildVegaConfig, readChartThemeTokens } from './vegaTheme';

const VARS: Record<string, string> = {
  '--chart-color-1': '#111111',
  '--chart-color-2': '#222222',
  '--chart-color-3': '#333333',
  '--chart-title': '#0a0a0a',
  '--chart-label': '#666666',
  '--chart-grid': 'rgba(0, 0, 0, 0.06)',
  '--chart-axis': 'rgba(0, 0, 0, 0.15)',
};

describe('readChartThemeTokens', () => {
  it('reads the palette and surface tokens from the theme variables', () => {
    const tokens = readChartThemeTokens((name) => VARS[name] ?? '', 'Inter, sans-serif');
    expect(tokens.colors).toEqual(['#111111', '#222222', '#333333']);
    expect(tokens.title).toBe('#0a0a0a');
    expect(tokens.label).toBe('#666666');
    expect(tokens.grid).toBe('rgba(0, 0, 0, 0.06)');
    expect(tokens.axis).toBe('rgba(0, 0, 0, 0.15)');
    expect(tokens.font).toBe('Inter, sans-serif');
  });

  it('falls back to a built-in palette when the variables are not defined', () => {
    const tokens = readChartThemeTokens(() => '', '');
    expect(tokens.colors).toHaveLength(12);
    expect(tokens.colors.every((c) => /^#[0-9A-F]{6}$/i.test(c))).toBe(true);
    expect(tokens.title).not.toBe('');
    expect(tokens.font).not.toBe('');
  });
});

describe('buildVegaConfig', () => {
  const tokens = readChartThemeTokens((name) => VARS[name] ?? '', 'Inter, sans-serif');
  interface ConfigShape {
    background: unknown;
    font: unknown;
    range: { category: unknown };
    mark: { tooltip: unknown };
    view: { stroke: unknown };
    axis: Record<string, unknown>;
    legend: Record<string, unknown>;
    title: Record<string, unknown>;
  }
  const config = buildVegaConfig(tokens) as unknown as ConfigShape;

  it('uses the palette as the categorical color range', () => {
    expect(config.range.category).toEqual(['#111111', '#222222', '#333333']);
  });

  it('turns tooltips on for every mark and keeps the card surface visible', () => {
    expect(config.mark.tooltip).toBe(true);
    expect(config.background).toBe('transparent');
    expect(config.view.stroke).toBeNull();
  });

  it('applies text and line tokens to axes, legends and titles', () => {
    expect(config.font).toBe('Inter, sans-serif');
    expect(config.axis.labelColor).toBe('#666666');
    expect(config.axis.titleColor).toBe('#0a0a0a');
    expect(config.axis.gridColor).toBe('rgba(0, 0, 0, 0.06)');
    expect(config.axis.domainColor).toBe('rgba(0, 0, 0, 0.15)');
    expect(config.legend.labelColor).toBe('#666666');
    expect(config.title.color).toBe('#0a0a0a');
    // Vega reads the subtitle colour from its own key; without this the
    // subtitle renders black whatever the theme (verified against the
    // rendered SVG), so the model's subtitles vanish on a dark card.
    expect(config.title.subtitleColor).toBe('#666666');
    // Pinned: Vega's default subtitle is the same 12px as the title, which
    // reads as a second heading rather than a caption.
    expect(config.title.subtitleFontSize).toBe(11);
  });
});
