import { afterEach, expect, it, vi } from 'vitest';
import { compile, type TopLevelSpec } from 'vega-lite';
import { parse, View } from 'vega';
import { withChartInteractions } from './vegaInteractions';
import { buildVegaConfig, readChartThemeTokens } from './vegaTheme';

let view: View | undefined;
afterEach(() => { view?.finalize(); document.body.replaceChildren(); vi.restoreAllMocks(); });

it.each([
  ['unit', { mark: 'point' }],
  ['flat layer', { layer: [{ mark: 'line' }, { mark: 'point' }] }],
])('handles legend, zoom and repeated Reset events on a %s chart', async (_name, marks) => {
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
  const labels = host.querySelectorAll('.role-legend-label text');
  expect(labels).toHaveLength(2);
  labels[0]!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await view.runAsync();
  expect(view.data('clai_auto_legend_store').map((tuple: { values: string[] }) => tuple.values)).toEqual([['A']]);

  labels[1]!.dispatchEvent(new MouseEvent('click', { bubbles: true, shiftKey: true }));
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
  expect(view.width()).toBe(450);
  labels[0]!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  point.dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, clientX: 150, deltaY: -100 }));
  await view.runAsync();
  expect(view.data('clai_auto_legend_store')).toHaveLength(1);
  expect(view.scale('x').domain()).not.toEqual(domain);
  svg.dispatchEvent(new Event(enhanced.resetEvent!));
  await view.runAsync();
  expect(view.data('clai_auto_legend_store')).toHaveLength(0);
  expect(view.scale('x').domain()).toEqual(domain);
  expect(error).not.toHaveBeenCalled();
});
