import { describe, expect, it } from 'vitest';
import { zoomGeometry, type ZoomStart } from './FleetZoom';

const VW = 1366;
const VH = 768;
/** A card bubble with its tail on `side`, the speaker's 36px face 16px beyond it. */
const start = (left: number, top: number, width: number, height: number, side: 'left' | 'right' = 'left', face = true): ZoomStart => {
  const faceLeft = side === 'left' ? left - 16 - 36 : left + width + 16;
  return {
    rect: { left, top, width, height },
    tailY: top + height / 2 - 11,
    side,
    source: null,
    look: {},
    face: face ? ({ left: faceLeft, top: top + height / 2 - 18, width: 36, height: 36, right: faceLeft + 36, bottom: top + height / 2 + 18 } as DOMRect) : null,
  };
};

describe('zoomGeometry', () => {
  it('widens away from the face on the bubble’s side when there is room, the tail where it was', () => {
    const s = start(120, 100, 300, 60);
    const end = zoomGeometry(s, 200, VW, VH);
    expect(end).toMatchObject({ side: 'left', left: 120, top: 100, height: 200 });
    expect(end.width).toBe(Math.min(560, VW * 0.6));
    expect(end.top + end.tailAt).toBe(s.tailY);
  });

  it('opens on the face’s other side in the right column, the tail at the same height on the face', () => {
    const s = start(1150, 400, 200, 60);
    const end = zoomGeometry(s, 200, VW, VH);
    expect(end.side).toBe('right');
    // Its right edge is the gap before the face.
    expect(end.left + end.width).toBe(s.face!.left - 16);
    expect(end.top + end.tailAt).toBe(s.tailY);
    // In the bottom half, it grows up from the bubble's foot.
    expect(end.top + end.height).toBe(460);
  });

  it('keeps your words, with no face, on their side', () => {
    const end = zoomGeometry(start(1000, 600, 300, 44, 'right', false), 100, VW, VH);
    expect(end.side).toBe('right');
    expect(end.left + end.width).toBe(1300);
  });

  it('is never taller than the window: a long message scrolls inside', () => {
    const end = zoomGeometry(start(120, 300, 300, 60), 5000, VW, VH);
    expect(end.height).toBe(VH - 32);
    expect(end.top).toBe(16);
  });
});
