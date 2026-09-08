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
    config: buildVegaConfig(readChartThemeTokens(() => ''), spec),
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
  opacity?: number | null;
  x?: number;
  y?: number;
  size?: number;
}
const markItems = (item: SceneItem): SceneItem[] => [
  ...(item.mark?.role === 'mark' ? [item] : []),
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

  it('binds continuous scales in a layer with native event-only gesture handlers', async () => {
    const { compiled, enhanced } = await render({
      ...POINTS, mark: undefined, layer: [{ mark: 'line' }, { mark: 'point' }],
    });
    expect(enhanced.canPanZoom).toBe(true);
    const zoom = compiled.signals?.find((signal) => signal.name === 'clai_auto_zoom_zoom_delta');
    const pan = compiled.signals?.find((signal) => signal.name === 'clai_auto_zoom_translate_anchor');
    expect(JSON.stringify(zoom)).toContain('"type":"wheel"');
    expect(JSON.stringify(pan)).toContain('"type":"pointerdown"');
    expect(compiled.scales?.filter((scale) => scale.domainRaw !== undefined)).toHaveLength(2);
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
    ['authored selection defaults', { ...POINTS, config: { selection: {
      point: { resolve: 'union' }, interval: { resolve: 'union' },
    } } }],
    ['custom canvas renderer', { ...POINTS, usermeta: { embedOptions: { renderer: 'canvas' } } }],
    ['custom embed config', { ...POINTS, usermeta: { embedOptions: { config: { mark: { opacity: 0 } } } } }],
    ['explicit opt out', { ...POINTS, usermeta: { clai: { interactions: false } } }],
  ])('leaves %s untouched', (_name, spec) => {
    expect(withChartInteractions(spec)).toEqual({ spec, legendFocus: false, canPanZoom: false, resetEvent: null });
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
      // Vega-Lite 6.4.3 compiles sqrt gestures with linear math, moving the
      // value under the pointer during zoom. Leave that axis unbound.
      { field: 'x', type: 'quantitative', scale: { type: 'sqrt' } },
      { field: 'x', type: 'quantitative', scale: { domain: [0, 10] } },
      { field: 'x', type: 'quantitative', scale: { domainMin: 0 } },
      { field: 'x', type: 'quantitative', scale: { domainMax: 10 } },
      { field: 'x', type: 'quantitative', scale: { domainMid: 5 } },
      { field: 'x', type: 'quantitative', stack: 'zero' },
    ]) {
      const result = withChartInteractions({ ...POINTS, encoding: { x } });
      expect(result.canPanZoom).toBe(false);
    }
  });

  it.each([
    { type: 'log' }, { type: 'pow', exponent: 0.5 }, { type: 'symlog', constant: 2 },
  ])('keeps the pointer anchor fixed for supported nonlinear scales: %j', async (scale) => {
    const { view } = await render({
      ...POINTS, data: { values: [{ x: 1 }, { x: 100 }] },
      encoding: { x: { field: 'x', type: 'quantitative', scale: { ...scale, nice: false } } },
    });
    const anchor = view.scale('x').invert(200);
    view.signal('clai_auto_zoom_zoom_anchor', { x: anchor });
    view.signal('clai_auto_zoom_zoom_delta', 0.5);
    await view.runAsync();
    expect(view.scale('x')(anchor)).toBeCloseTo(200);
  });

  it.each(['bar', 'area'])('keeps stacked %s baselines on the plot floor', async (mark) => {
    const { view, compiled } = await render({ ...POINTS, mark });
    expect(view.scale('y')(0)).toBe(view.height());
    // Rounded stack segments would leave notches at their joins.
    expect(JSON.stringify(compiled.marks)).not.toContain('cornerRadius');
  });

  it.each([
    { encoding: { ...POINTS.encoding, x: { ...POINTS.encoding.x, scale: { padding: 0 } } } },
    { config: { scale: { continuousPadding: 0 } } },
  ])('preserves authored scale padding: %j', async (properties) => {
    const { view } = await render({ ...POINTS, ...properties });
    expect(view.scale('x')(0)).toBe(0);
  });

  it.each([0.05, 0.5])('preserves authored global mark opacity %s', async (opacity) => {
    const { view } = await render({ ...POINTS, config: { mark: { opacity } } });
    const items = markItems((view.scenegraph() as unknown as { root: SceneItem }).root);
    expect(items).toHaveLength(4);
    expect(items.every((item) => item.opacity === opacity)).toBe(true);
  });

  it.each([
    ['line', { strokeWidth: 0, strokeCap: 'butt', strokeJoin: 'miter' }, { strokeWidth: 0, strokeCap: 'butt', strokeJoin: 'miter' }],
    ['point', { filled: false, size: 5 }, { fill: 'transparent', size: 5 }],
    ['circle', { size: 5 }, { size: 5 }],
    ['square', { size: 5 }, { size: 5 }],
    ['text', { color: '#123456' }, { fill: '#123456' }],
    ['rule', { color: '#123456' }, { stroke: '#123456' }],
  ])('preserves authored global %s styling', async (mark, authored, expected) => {
    const { view } = await render({ ...POINTS, mark, config: { mark: authored }, encoding: {
      x: POINTS.encoding.x, y: POINTS.encoding.y, ...(mark === 'text' ? { text: { field: 'series' } } : {}),
    } });
    const items = markItems((view.scenegraph() as unknown as { root: SceneItem }).root);
    expect(items.length).toBeGreaterThan(0);
    items.forEach((item) => expect(item).toMatchObject(expected));
  });

  it('retains native opacity for aggregated symbols', async () => {
    const { view } = await render({ ...POINTS, encoding: {
      ...POINTS.encoding, y: { ...POINTS.encoding.y, aggregate: 'sum' },
    } });
    const items = markItems((view.scenegraph() as unknown as { root: SceneItem }).root);
    expect(items).toHaveLength(4);
    expect(items.every((item) => (item.opacity ?? 1) === 1)).toBe(true);
  });

  it('ignores composition-like data keys and avoids named-data collisions', async () => {
    const { enhanced } = await render({
      ...POINTS,
      data: { name: 'clai_auto_legend_store', values: [
        { x: 1, y: 2, series: 'A', layer: [1, 2], params: [{ select: 'point' }] },
      ] },
    });
    expect(enhanced.legendFocus).toBe(true);
    expect(enhanced.canPanZoom).toBe(true);
  });

  it.each([
    { mark: { type: 'point', opacity: 0.05 } },
    { mark: { type: 'point', opacity: { expr: '0.05' } } },
    { mark: { type: 'line', point: 'transparent' } },
    { mark: { type: 'line', point: { opacity: 0 } } },
    { config: { point: { opacity: 0.05 } } },
    { config: { mark: { opacity: 0.05 } } },
    { mark: { type: 'point', style: 'faint' }, config: { style: { faint: { opacity: 0.05 } } } },
  ])('does not brighten faint or transparent marks with legend focus: %j', async (properties) => {
    const { enhanced, view } = await render({ ...POINTS, ...properties });
    expect(enhanced.legendFocus).toBe(false);
    const points = markItems((view.scenegraph() as unknown as { root: SceneItem }).root);
    expect(points.some((point) => point.opacity != null && point.opacity < 0.12)).toBe(true);
  });

  it('keeps default marks visible and padded inside the clipping boundary, including after zoom', async () => {
    const { view } = await render({ ...POINTS, data: { values: [
      { x: 0, y: 0, series: 'A' }, { x: 100, y: 100, series: 'B' },
    ] } });
    const items = markItems((view.scenegraph() as unknown as { root: SceneItem }).root);
    expect(items).toHaveLength(2);
    for (const point of items) {
      expect(point.opacity).toBe(0.7);
      expect(point.x).toBeGreaterThan(Math.sqrt(point.size ?? 0));
      expect(point.x).toBeLessThan(500 - Math.sqrt(point.size ?? 0));
      expect(point.y).toBeGreaterThan(Math.sqrt(point.size ?? 0));
      expect(point.y).toBeLessThan(260 - Math.sqrt(point.size ?? 0));
    }
    view.signal('clai_auto_zoom_x', [20, 80]);
    view.signal('clai_auto_zoom_y', [20, 80]);
    await view.runAsync();
    // Out-of-domain points remain clipped; the chart cannot grow into its axes.
    expect(await view.toSVG()).toContain('clip-path="url(#');
  });

  it.each(['line', 'rule', 'text'])('keeps a default %s visible before any legend click', async (mark) => {
    const { view } = await render({ ...POINTS, mark });
    const items = markItems((view.scenegraph() as unknown as { root: SceneItem }).root);
    expect(items.length).toBeGreaterThan(0);
    expect(items.every((item) => (item.opacity ?? 1) === 1)).toBe(true);
  });

  it.each([undefined, 'right'])('keeps a twelve-series legend compact (orient %s)', async (orient) => {
    const { view } = await render({ ...POINTS,
      data: { values: Array.from({ length: 12 }, (_,i) => ({ x: i, y: i, series: `Series ${i}` })) },
      encoding: { ...POINTS.encoding, color: { ...POINTS.encoding.color, legend: { orient } } },
    });
    const svg = await view.toSVG();
    expect(Number(svg.match(/<svg[^>]* width="(\d+)"/)?.[1])).toBeLessThan(760);
    expect(svg).toContain('Series 11');
  });

  it.each(['rule', 'text'])('themes unencoded %s marks for a dark card', async (mark) => {
    const tokens = { ...readChartThemeTokens(() => ''), label: '#ccddee' };
    const compiled = compile({
      data: POINTS.data, mark, encoding: { y: POINTS.encoding.y, ...(mark === 'text' ? { text: { field: 'series' } } : {}) },
    } as TopLevelSpec, { config: buildVegaConfig(tokens) }).spec;
    const view = new View(parse(compiled), { renderer: 'none' });
    views.push(view);
    await view.runAsync();
    expect(await view.toSVG()).toContain(mark === 'rule' ? 'stroke="#ccddee"' : 'fill="#ccddee"');
  });
});
