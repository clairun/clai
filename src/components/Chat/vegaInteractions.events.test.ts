import { afterEach, expect, it, vi } from 'vitest';
import { compile, type TopLevelSpec } from 'vega-lite';
import { loader, logger, parse, View, Warn } from 'vega';
import { withChartInteractions } from './vegaInteractions';
import { buildVegaConfig, readChartThemeTokens } from './vegaTheme';

let view: View | undefined;
afterEach(() => { view?.finalize(); document.body.replaceChildren(); vi.restoreAllMocks(); });

it('reports malformed JSON data through Vega\'s ingestion warning contract', async () => {
  const compiled = compile({
    data: { url: 'bad.json', format: { type: 'json' } },
    mark: 'point',
    encoding: {
      x: { field: 'x', type: 'quantitative' },
      y: { field: 'y', type: 'quantitative' },
    },
  }).spec;
  const warnings: unknown[][] = [];
  const dataLoader = loader();
  dataLoader.load = async () => '{]';
  view = new View(parse(compiled), {
    loader: dataLoader,
    logger: logger(Warn, undefined, (_method, _level, args) => warnings.push(args)),
    renderer: 'none',
  });

  await view.runAsync();

  expect(warnings.some((args) => args[0] === 'Data ingestion failed' && args[1] === 'bad.json')).toBe(true);
  expect(view.data('source_0')).toHaveLength(0);
});

it.each([
  ['unit', { mark: 'point' }],
  ['flat layer', { layer: [{ mark: 'line' }, { mark: 'point' }] }],
])('handles legend, zoom and repeated Reset events on a %s chart', async (name, marks) => {
  const enhanced = withChartInteractions({
    width: 400, height: 240,
    data: { values: [
      { x: 1, y: 2, series: 'A' }, { x: 2, y: 4, series: 'A' },
      { x: 1, y: 3, series: 'B' }, { x: 2, y: 6, series: 'B' },
    ] },
    ...marks,
    encoding: {
      x: { field: 'x', type: 'quantitative' },
      y: { field: 'y', type: 'quantitative' },
      color: { field: 'series', type: 'nominal' },
    },
  });
  const compiled = compile(enhanced.spec as unknown as TopLevelSpec, {
    config: buildVegaConfig(readChartThemeTokens(() => ''), enhanced.spec),
  }).spec;
  const host = document.createElement('div');
  document.body.appendChild(host);
  view = new View(parse(compiled), { renderer: 'svg' });
  const error = vi.spyOn(console, 'error');
  await view.initialize(host).runAsync();
  const markOpacities = () => Array.from(host.querySelectorAll('.role-mark path'),
    (node) => Number(node.getAttribute('opacity') ?? 1));
  const initialOpacities = markOpacities();
  expect(initialOpacities.length).toBeGreaterThan(0);
  expect(initialOpacities.every((opacity) => opacity >= 0.7)).toBe(true);
  const labels = host.querySelectorAll('.role-legend-label text');
  expect(labels).toHaveLength(2);
  labels[0]!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await view.runAsync();
  expect(view.data('clai_auto_legend_store').map((tuple: { values: string[] }) => tuple.values)).toEqual([['A']]);
  expect(markOpacities()).toContain(0.12);

  labels[1]!.dispatchEvent(new MouseEvent('click', { bubbles: true, shiftKey: true }));
  await view.runAsync();
  expect(view.data('clai_auto_legend_store').map((tuple: { values: string[] }) => tuple.values)).toEqual([['B']]);

  labels[0]!.dispatchEvent(new MouseEvent('click', {
    bubbles: true,
    ...(name === 'unit' ? { ctrlKey: true } : { metaKey: true }),
  }));
  await view.runAsync();
  expect(view.data('clai_auto_legend_store')).toHaveLength(2);

  const svg = host.querySelector('svg')!;
  const point = host.querySelector('.mark-symbol.role-mark path')!;
  point.dispatchEvent(new MouseEvent('pointermove', { bubbles: true, clientX: 150, clientY: 100 }));
  await view.runAsync();
  const domain = view.scale('x').domain();
  point.dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, clientX: 150, deltaY: -100 }));
  await view.runAsync();
  const zoomed = view.scale('x').domain();
  expect(zoomed[1] - zoomed[0]).toBeLessThan(domain[1] - domain[0]);

  // Browsers emit a click on the SVG background after a drag-pan.
  // Vega's default bind: 'legend' clears its selection on that click.
  svg.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await view.runAsync();
  expect(view.data('clai_auto_legend_store')).toHaveLength(2);
  // A reset restores the selection state without restoring layout dimensions.
  view.width(450);
  await view.runAsync();
  svg.dispatchEvent(new Event(enhanced.resetEvent!));
  await view.runAsync();
  expect(view.scale('x').domain()).toEqual(domain);
  expect(view.data('clai_auto_legend_store')).toHaveLength(0);
  expect(markOpacities()).toEqual(initialOpacities);
  expect(view.width()).toBe(450);
  labels[0]!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  point.dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, clientX: 150, deltaY: -100 }));
  await view.runAsync();
  expect(view.data('clai_auto_legend_store')).toHaveLength(1);
  expect(view.scale('x').domain()).not.toEqual(domain);
  svg.dispatchEvent(new Event(enhanced.resetEvent!));
  await view.runAsync();
  expect(view.data('clai_auto_legend_store')).toHaveLength(0);
  expect(markOpacities()).toEqual(initialOpacities);
  expect(view.scale('x').domain()).toEqual(domain);
  expect(error).not.toHaveBeenCalled();
});
