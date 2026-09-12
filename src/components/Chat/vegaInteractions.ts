/**
 * Add native Vega-Lite interactions to ordinary unit / flat layered charts.
 * Selections live on exactly one unit, never above a layer. Composed views
 * and authored params remain under the author's control.
 *
 * Only the render copy changes; saved specs, transforms and data URLs stay
 * portable. Set usermeta.clai.interactions = false to opt out.
 */
type ObjectValue = Record<string, unknown>;
const object = (value: unknown): ObjectValue =>
  typeof value === 'object' && value !== null && !Array.isArray(value) ? value as ObjectValue : {};

const SIMPLE_MARKS = new Set([
  'arc', 'area', 'bar', 'circle', 'line', 'point', 'rect', 'rule', 'square', 'text', 'tick', 'trail',
]);
const ZOOM_MARKS = new Set(['circle', 'line', 'point', 'square', 'trail']);
const DIMMED_OPACITY = 0.12;
// Vega-Lite 6.4.3 uses linear gesture math for sqrt scales, so omit sqrt.
const CONTINUOUS_SCALES = new Set(['linear', 'log', 'pow', 'symlog', 'time', 'utc']);

export interface ChartInteractions {
  spec: ObjectValue;
  legendFocus: boolean;
  /** The host gates these gestures until the user enables pan/zoom. */
  canPanZoom: boolean;
  /** Native clear event dispatched on the displayed SVG by the host. */
  resetEvent: string | null;
}

export const withChartInteractions = (spec: ObjectValue): ChartInteractions => {
  const unchanged: ChartInteractions = { spec, legendFocus: false, canPanZoom: false, resetEvent: null };
  if (object(object(spec.usermeta).clai).interactions === false) return unchanged;
  // Embed options can change the renderer, config or compiled spec after this
  // pass. Preserve those charts' authored behavior instead of guessing it here.
  if (object(spec.usermeta).embedOptions !== undefined) return unchanged;

  const config = object(spec.config);
  if (config.selection !== undefined) return unchanged;

  const units = Array.isArray(spec.layer) ? spec.layer.map(object) : [spec];
  const views = [spec, ...units];
  if (units.length === 0 || views.some((view) =>
    view.facet !== undefined || view.repeat !== undefined || view.concat !== undefined
    || view.hconcat !== undefined || view.vconcat !== undefined || view.resolve !== undefined
    || (Array.isArray(view.params) && view.params.length > 0)
  )) return unchanged;
  const marks = units.map((unit) => typeof unit.mark === 'string' ? unit.mark : object(unit.mark).type);
  if (marks.some((mark) => typeof mark !== 'string' || !SIMPLE_MARKS.has(mark))) return unchanged;
  const encodings = units.map((unit) => ({ ...object(spec.encoding), ...object(unit.encoding) }));
  if (encodings.some((encoding) => ['row', 'column', 'facet'].some((key) => key in encoding))) {
    return unchanged;
  }

  const colors = encodings.map((encoding) => object(encoding.color));
  const firstColor = object(colors[0]);
  // Let Vega retain opacity precedence. Skip faint, expression-based or
  // transparent overlay marks: a fixed dimming value could reveal them.
  const opacityStyles = [config.mark, ...Object.values(object(config.style)),
    ...[...SIMPLE_MARKS].map((mark) => config[mark]), ...units.map((unit) => unit.mark)];
  const canDim = opacityStyles.every((style) => {
    const props = object(style);
    return props.point !== 'transparent' && [props, object(props.point), object(props.line)].every((part) =>
      part.opacity === undefined || (typeof part.opacity === 'number' && part.opacity >= DIMMED_OPACITY)
    );
  });
  const legendFocus = canDim && object(config.legend).disable !== true && colors.every((color, index) =>
    typeof color.field === 'string' && color.field === firstColor.field
    && (color.type === 'nominal' || color.type === 'ordinal') && color.type === firstColor.type
    && color.condition === undefined && color.aggregate === undefined && !color.bin && !color.timeUnit
    && color.legend !== null && color.legend !== false && object(color.legend).type !== 'gradient'
    && color.scale !== null && (object(color.scale).type === undefined || object(color.scale).type === 'ordinal')
    // Opacity may encode data or an authored condition; never take it over.
    && object(encodings[index]).opacity === undefined
  );

  // Binding scales is meaningful for continuous positions, not categories,
  // bins, stacked totals, projections or explicitly unclipped marks.
  const zoomChannels = (['x', 'y'] as const).filter((channel) =>
    units.every((unit, index) => {
      const field = object(object(encodings[index])[channel]);
      const scale = object(field.scale);
      const first = object(object(encodings[0])[channel]);
      return ZOOM_MARKS.has(marks[index] as string) && !unit.projection && !spec.projection
        && object(unit.mark).clip !== false
        && typeof field.field === 'string' && field.field === first.field
        && (field.type === 'quantitative' || field.type === 'temporal') && field.type === first.type
        && !field.bin && !field.timeUnit && !field.aggregate && !field.stack
        && field.scale !== null && (scale.type === undefined || CONTINUOUS_SCALES.has(String(scale.type)))
        && ['domain', 'domainRaw', 'domainMin', 'domainMax', 'domainMid'].every((key) => scale[key] === undefined)
        && object(encodings[index])[`${channel}2`] === undefined;
    })
  );
  if (!legendFocus && zoomChannels.length === 0) return unchanged;

  // Also avoid collisions with named datasets / views, not just params.
  const source = JSON.stringify(spec);
  let prefix = 'clai_auto';
  while (source.includes(prefix)) prefix += '_';
  const legendName = `${prefix}_legend`;
  const zoomName = `${prefix}_zoom`;
  const resetEvent = `${prefix}_reset`;
  const canPanZoom = zoomChannels.length > 0;
  const params: ObjectValue[] = [];
  if (legendFocus) {
    params.push({
      name: legendName,
      select: {
        type: 'point',
        fields: [firstColor.field],
        // Shift-click also extends the browser's text selection over SVG
        // labels. Use the platform's additive-selection modifier instead.
        toggle: 'event.ctrlKey || event.metaKey',
      },
      // Vega's default legend binding also clears on background clicks,
      // including the click dispatched after a drag-pan. Reset is explicit.
      bind: { legend: `click[event.item && indexof(event.item.mark.role, 'legend') >= 0], ${resetEvent}` },
    });
  }
  if (canPanZoom) {
    params.push(
      {
        name: zoomName,
        select: {
          type: 'interval',
          encodings: zoomChannels,
          // The host gates these events in capture phase. Vega event filters
          // cannot reference signals (they compile but fail when dispatched).
          translate: '[pointerdown, window:pointerup] > window:pointermove!',
          zoom: 'wheel!',
          clear: resetEvent,
        },
        bind: 'scales',
      },
    );
  }

  const enhanced = units.map((unit, index) => {
    const encoding = { ...encodings[index] };
    if (legendFocus) {
      encoding.opacity = {
        // No fallback value: Vega-Lite retains the mark/config's original
        // opacity for selected marks (and for the initial, empty selection).
        condition: { test: { not: { param: legendName } }, value: DIMMED_OPACITY },
      };
    }
    // Vega-Lite clips marks when scales are bound. Pad only these axes;
    // global continuous padding would detach bar/area baselines from zero.
    for (const channel of zoomChannels) {
      const field = object(encoding[channel]);
      if (object(field.scale).padding === undefined && object(config.scale).continuousPadding === undefined) {
        encoding[channel] = { ...field, scale: { ...object(field.scale), padding: 8 } };
      }
    }
    return { ...unit, encoding, ...(index === 0 ? { params } : {}) };
  });
  return {
    spec: Array.isArray(spec.layer) ? { ...spec, layer: enhanced } : enhanced[0] ?? spec,
    legendFocus,
    canPanZoom,
    resetEvent,
  };
};
