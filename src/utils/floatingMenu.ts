import type { CSSProperties } from 'react';

type AnchorRect = Pick<DOMRectReadOnly, 'top' | 'bottom' | 'right'>;

type Viewport = {
  width: number;
  height: number;
};

export type FixedRightAlignedMenuOptions = {
  estimatedHeight: number;
  gap: number;
  margin: number;
  minWidth: number;
};

export const fixedRightAlignedMenuStyleFromRect = (
  rect: AnchorRect,
  viewport: Viewport,
  options: FixedRightAlignedMenuOptions
): CSSProperties => {
  const spaceBelow = viewport.height - rect.bottom;
  const flipUp = spaceBelow < options.estimatedHeight && rect.top > spaceBelow;
  const right = Math.min(
    Math.max(viewport.width - rect.right, options.margin),
    Math.max(viewport.width - options.minWidth - options.margin, options.margin)
  );
  const maxHeight = Math.max((flipUp ? rect.top : spaceBelow) - options.gap - options.margin, 0);

  return {
    position: 'fixed',
    right,
    maxHeight,
    ...(flipUp
      ? { bottom: viewport.height - rect.top + options.gap }
      : { top: rect.bottom + options.gap }),
  };
};

export const fixedRightAlignedMenuStyle = (
  trigger: HTMLElement,
  options: FixedRightAlignedMenuOptions
): CSSProperties =>
  fixedRightAlignedMenuStyleFromRect(
    trigger.getBoundingClientRect(),
    { width: window.innerWidth, height: window.innerHeight },
    options
  );

export const sameFloatingMenuStyle = (a: CSSProperties, b: CSSProperties): boolean =>
  a.top === b.top && a.bottom === b.bottom && a.right === b.right && a.maxHeight === b.maxHeight;
