// @vitest-environment node
import { afterEach, describe, expect, it } from 'vitest';
import { compile, type TopLevelSpec } from 'vega-lite';
import { loader, parse, View } from 'vega';
import { withChartInteractions } from './vegaInteractions';
import { buildVegaConfig, readChartThemeTokens } from './vegaTheme';

const POINTS = {
  width: 500,
  height: 260,
  data: { values: [
    { x: 1, y: 3, series: 'A' }, { x: 2, y: 5, series: 'A' },
    { x: 1, y: 7, series: 'B' }, { x: 2, y: 4, series: 'B' },
  ] },
  mark: 'point',
  encoding: {
    x: { field: 'x', type: 'quantitative' },
    y: { field: 'y', type: 'quantitative' },
    color: { field: 'series', type: 'nominal' },
  },
};
const views: View[] = [];
afterEach(() => views.splice(0).forEach((view) => view.finalize()));

const render = async (spec: Record<string, unknown>) => {
  const enhanced = withChartInteractions(spec);
  const compiled = compile(enhanced.spec as unknown as TopLevelSpec, {
    config: buildVegaConfig(readChartThemeTokens(() => '')),
  }).spec;
  const view = new View(parse(compiled), { renderer: 'none' });
  views.push(view);
  await view.runAsync();
  return { view, compiled, enhanced };
};

interface SceneItem {
  items?: SceneItem[];
  mark?: { role?: string };
  datum?: { series?: string };
  opacity?: number;
}
const markItems = (item: SceneItem): SceneItem[] => [
  ...(item.mark?.role === 'mark' && item.opacity !== undefined ? [item] : []),
  ...(item.items ?? []).flatMap(markItems),
];

describe('automatic chart interactions, using the installed Vega runtime', () => {
  it('focuses a series without filtering rows, changing domains or losing the other legend entries', async () => {
    const { view } = await render({ ...POINTS, mark: { type: 'point', opacity: 0.8 } });
    const xDomain = view.scale('x').domain();
    const yDomain = view.scale('y').domain();
    view.signal('clai_auto_legend_series_legend', 'A');
    await view.runAsync();
    const points = markItems((view.scenegraph() as unknown as { root: SceneItem }).root);
    expect(points.map((point) => [point.datum?.series, point.opacity])).toEqual([
      ['A', 0.8], ['A', 0.8], ['B', 0.12], ['B', 0.12],
    ]);
    expect(view.scale('x').domain()).toEqual(xDomain);
    expect(view.scale('y').domain()).toEqual(yDomain);
    expect(view.scale('color').domain()).toEqual(['A', 'B']);
    expect(view.data('source_0')).toHaveLength(4);
    expect(await view.toSVG()).not.toMatch(/NaN|undefined/);

    // The same store used by Shift-click can contain multiple series.
    view.signal('clai_auto_legend_toggle', true);
    view.signal('clai_auto_legend_series_legend', 'B');
    await view.runAsync();
    expect(markItems((view.scenegraph() as unknown as { root: SceneItem }).root).every((point) => point.opacity === 0.8)).toBe(true);
    view.signal('clai_auto_legend_toggle', false);
    view.signal('clai_auto_legend_series_legend', null);
    await view.runAsync();
    expect(view.data('clai_auto_legend_store')).toHaveLength(0);
  });

  it.each([
    ['line', { ...POINTS, mark: 'line' }],
    ['line with points', { ...POINTS, mark: { type: 'line', point: true } }],
    ['flat layer', { ...POINTS, mark: undefined, layer: [{ mark: 'line' }, { mark: 'point' }] }],
    ['stacked area', { ...POINTS, mark: 'area', encoding: {
      ...POINTS.encoding, y: { ...POINTS.encoding.y, stack: 'center' },
    } }],
    ['bar', { ...POINTS, mark: 'bar', encoding: {
      ...POINTS.encoding, x: { ...POINTS.encoding.x, type: 'ordinal' },
    } }],
    ['arc', { data: POINTS.data, mark: 'arc', encoding: {
      theta: { field: 'y', type: 'quantitative' }, color: POINTS.encoding.color,
    } }],
  ])('compiles, parses and runs a %s with one legend selection', async (_name, spec) => {
    const { view, compiled, enhanced } = await render(spec);
    expect(enhanced.legendFocus).toBe(true);
    expect(compiled.signals?.filter((signal) => signal.name === 'clai_auto_legend')).toHaveLength(1);
    view.signal('clai_auto_legend_series_legend', 'B');
    await view.runAsync();
    expect(view.data('clai_auto_legend_store')).toHaveLength(1);
    expect(await view.toSVG()).not.toMatch(/NaN|undefined/);
  });

  it('gates wheel and drag events with an initially disabled signal, including in a layer', async () => {
    const { view, compiled, enhanced } = await render({
      ...POINTS, mark: undefined, layer: [{ mark: 'line' }, { mark: 'point' }],
    });
    expect(enhanced.panZoomSignal).toBe('clai_auto_pan_enabled');
    expect(view.signal('clai_auto_pan_enabled')).toBe(false);
    const zoom = compiled.signals?.find((signal) => signal.name === 'clai_auto_zoom_zoom_delta');
    const pan = compiled.signals?.find((signal) => signal.name === 'clai_auto_zoom_translate_anchor');
    expect(JSON.stringify(zoom)).toContain('"filter":["clai_auto_pan_enabled"]');
    expect(JSON.stringify(pan)).toContain('"filter":["clai_auto_pan_enabled"]');
    view.signal('clai_auto_pan_enabled', true);
    await view.runAsync();
    expect(view.signal('clai_auto_pan_enabled')).toBe(true);
  });

  it('preserves URL-backed data and transforms and never mutates the saved spec', async () => {
    const spec = {
      ...POINTS,
      data: { url: '/data/sales.csv' },
      transform: [{ calculate: 'datum.y * 2', as: 'doubled' }],
      usermeta: { source: 'report' },
    };
    const original = structuredClone(spec);
    const enhanced = withChartInteractions(spec);
    expect(spec).toEqual(original);
    expect(enhanced.spec.data).toEqual(spec.data);
    expect(enhanced.spec.transform).toEqual(spec.transform);
    const compiled = compile(enhanced.spec as unknown as TopLevelSpec).spec;
    const loads: string[] = [];
    const view = new View(parse(compiled), {
      renderer: 'none',
      loader: {
        ...loader(),
        load: async (url: string) => { loads.push(url); return 'x,y,series\n1,3,A\n2,4,B'; },
      },
    });
    views.push(view);
    await view.runAsync();
    expect(loads).toEqual(['/data/sales.csv']);
    expect(view.data('source_0')).toHaveLength(2);
  });

  it.each([
    ['existing root params', { ...POINTS, params: [{ name: 'brush', select: 'interval' }] }],
    ['existing child params', { ...POINTS, mark: undefined, layer: [
      { mark: 'point', params: [{ name: 'brush', select: 'interval' }] }, { mark: 'line' },
    ] }],
    ['concat', { hconcat: [POINTS, POINTS] }],
    ['facet', { data: POINTS.data, facet: { field: 'series' }, spec: POINTS }],
    ['repeat', { repeat: ['x', 'y'], spec: POINTS }],
    ['nested layers', { layer: [{ layer: [POINTS, POINTS] }] }],
    ['encoding facet', { ...POINTS, encoding: { ...POINTS.encoding, column: { field: 'series' } } }],
    ['independent scales', { ...POINTS, resolve: { scale: { x: 'independent' } } }],
    ['composite mark', { ...POINTS, mark: 'boxplot' }],
    ['explicit opt out', { ...POINTS, usermeta: { clai: { interactions: false } } }],
  ])('leaves %s untouched', (_name, spec) => {
    expect(withChartInteractions(spec)).toEqual({ spec, legendFocus: false, panZoomSignal: null });
    expect(withChartInteractions(spec).spec).toBe(spec);
  });

  it.each([
    { color: { ...POINTS.encoding.color, legend: null } },
    { color: { field: 'y', type: 'quantitative' } },
    { color: { ...POINTS.encoding.color, scale: null } },
    { color: { ...POINTS.encoding.color, condition: { test: 'datum.x > 1', value: 'red' } } },
    { opacity: { field: 'y', type: 'quantitative' } },
    { opacity: { value: 0.5 } },
  ])('does not replace a hidden/continuous legend or an opacity encoding: %j', (encoding) => {
    const result = withChartInteractions({ ...POINTS, encoding: { ...POINTS.encoding, ...encoding } });
    expect(result.legendFocus).toBe(false);
    expect((result.spec.encoding as Record<string, unknown>).opacity).toEqual(encoding.opacity);
  });

  it('does not bind categorical, binned, fixed-domain or stacked axes', () => {
    for (const x of [
      { field: 'x', type: 'nominal' },
      { field: 'x', type: 'quantitative', bin: true },
      { field: 'x', type: 'quantitative', scale: { domain: [0, 10] } },
      { field: 'x', type: 'quantitative', stack: 'zero' },
    ]) {
      const result = withChartInteractions({ ...POINTS, encoding: { x } });
      expect(result.panZoomSignal).toBeNull();
    }
  });

  it('ignores composition-like data keys and avoids named-data collisions', async () => {
    const { enhanced } = await render({
      ...POINTS,
      data: { name: 'clai_auto_legend_store', values: [
        { x: 1, y: 2, series: 'A', layer: [1, 2], params: [{ select: 'point' }] },
      ] },
    });
    expect(enhanced.legendFocus).toBe(true);
    expect(enhanced.panZoomSignal).not.toBe('clai_auto_pan_enabled');
  });
});
