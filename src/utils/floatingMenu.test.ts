import { describe, expect, it } from 'vitest';

import { fixedRightAlignedMenuStyleFromRect, sameFloatingMenuStyle } from './floatingMenu';

const options = {
  estimatedHeight: 144,
  gap: 4,
  margin: 8,
  minWidth: 140,
};

describe('fixedRightAlignedMenuStyleFromRect', () => {
  it('drops below the trigger when there is room', () => {
    expect(
      fixedRightAlignedMenuStyleFromRect(
        { top: 100, bottom: 116, right: 240 },
        { width: 1024, height: 768 },
        options
      )
    ).toEqual({
      position: 'fixed',
      right: 784,
      top: 120,
      maxHeight: 640,
    });
  });

  it('flips above the trigger near the bottom of the viewport', () => {
    expect(
      fixedRightAlignedMenuStyleFromRect(
        { top: 744, bottom: 760, right: 240 },
        { width: 1024, height: 768 },
        options
      )
    ).toEqual({
      position: 'fixed',
      right: 784,
      bottom: 28,
      maxHeight: 732,
    });
  });

  it('caps height to the roomier side in a short viewport', () => {
    expect(
      fixedRightAlignedMenuStyleFromRect(
        { top: 90, bottom: 106, right: 240 },
        { width: 1024, height: 200 },
        options
      )
    ).toEqual({
      position: 'fixed',
      right: 784,
      top: 110,
      maxHeight: 82,
    });
  });

  it('keeps the menu inside the viewport right margin', () => {
    expect(
      fixedRightAlignedMenuStyleFromRect(
        { top: 40, bottom: 56, right: 1022 },
        { width: 1024, height: 768 },
        options
      ).right
    ).toBe(8);
  });

  it('keeps enough room for the menu minimum width on narrow anchors', () => {
    expect(
      fixedRightAlignedMenuStyleFromRect(
        { top: 40, bottom: 56, right: 20 },
        { width: 320, height: 768 },
        options
      ).right
    ).toBe(172);
  });
});

describe('sameFloatingMenuStyle', () => {
  it('matches only placement-relevant fields', () => {
    expect(
      sameFloatingMenuStyle(
        { position: 'fixed', right: 8, top: 20, maxHeight: 100, zIndex: 1 },
        { position: 'fixed', right: 8, top: 20, maxHeight: 100, zIndex: 2 }
      )
    ).toBe(true);
    expect(
      sameFloatingMenuStyle(
        { position: 'fixed', right: 8, top: 20, maxHeight: 100 },
        { position: 'fixed', right: 8, bottom: 20, maxHeight: 100 }
      )
    ).toBe(false);
  });
});
