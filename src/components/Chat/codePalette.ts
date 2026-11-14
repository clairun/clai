import type { CSSProperties } from 'react';
import { oneDark, oneLight } from 'react-syntax-highlighter/dist/esm/styles/prism';
import type { ResolvedTheme } from '../../theme';

type PrismStyle = Record<string, CSSProperties>;

// Token colours that miss 4.5:1 on the code surface (--color-bg-tertiary) or
// on the page, keyed by their One Dark / One Light value. Same hues, darker or
// lighter until they pass.
const DARK_RETINT: Record<string, string> = {
  'hsl(220, 10%, 40%)': '#8B919C', // comment
};

const LIGHT_RETINT: Record<string, string> = {
  'hsl(230, 4%, 64%)': '#646770', // comment
  'hsl(119, 34%, 47%)': '#326E33', // string
  'hsl(221, 87%, 60%)': '#2F5FC8', // function, variable, operator
  'hsl(35, 99%, 36%)': '#8A5A00', // number, constant, class name
  'hsl(5, 74%, 59%)': '#C82C1E', // tag, property
  'hsl(198, 99%, 37%)': '#016E9D', // url
};

// The surface comes from the stylesheet, not the palette, so the block's
// background, shadow and font follow the app theme and tokens.
const SURFACE_KEYS = new Set(['background', 'textShadow', 'fontFamily']);

const derive = (base: PrismStyle, retint: Record<string, string>): PrismStyle =>
  Object.fromEntries(
    Object.entries(base).map(([selector, rules]) => {
      const next: CSSProperties = {};
      for (const [prop, value] of Object.entries(rules)) {
        if (selector.includes('language-') && SURFACE_KEYS.has(prop)) continue;
        (next as Record<string, unknown>)[prop] =
          prop === 'color' && typeof value === 'string' ? (retint[value] ?? value) : value;
      }
      return [selector, next];
    }),
  );

const PALETTES: Record<ResolvedTheme, PrismStyle> = {
  dark: derive(oneDark, DARK_RETINT),
  light: derive(oneLight, LIGHT_RETINT),
};

/** Prism syntax colours for the app theme: One Dark or One Light, AA on the code surface. */
export const codePalette = (theme: ResolvedTheme): PrismStyle => PALETTES[theme];
