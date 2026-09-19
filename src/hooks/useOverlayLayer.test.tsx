import { describe, expect, it, vi } from 'vitest';
import { render } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import { useOverlayLayer, type OverlayLayerOptions } from './useOverlayLayer';

/** One overlay: mounted only while open, so its cleanup is the real one. */
const Layer = ({
  open,
  onEscape,
  options,
}: {
  open: boolean;
  onEscape: () => void;
  options?: OverlayLayerOptions;
}) => {
  useOverlayLayer(open, onEscape, options);
  return null;
};

describe('useOverlayLayer', () => {
  it('unwinds three stacked overlays one Escape at a time, unlocking only at the end', async () => {
    const user = userEvent.setup();
    const escapes = { bottom: vi.fn(), middle: vi.fn(), top: vi.fn() };

    const { rerender } = render(
      <>
        <Layer open onEscape={escapes.bottom} />
        <Layer open onEscape={escapes.middle} />
        <Layer open onEscape={escapes.top} />
      </>
    );
    expect(document.body.style.overflow).toBe('hidden');

    await user.keyboard('{Escape}');
    expect(escapes.top).toHaveBeenCalledTimes(1);
    expect(escapes.middle).not.toHaveBeenCalled();
    expect(escapes.bottom).not.toHaveBeenCalled();

    rerender(
      <>
        <Layer open onEscape={escapes.bottom} />
        <Layer open onEscape={escapes.middle} />
        <Layer open={false} onEscape={escapes.top} />
      </>
    );
    await user.keyboard('{Escape}');
    expect(escapes.middle).toHaveBeenCalledTimes(1);
    expect(escapes.bottom).not.toHaveBeenCalled();
    // Two of three gone and the page is still locked: the last one out unlocks.
    expect(document.body.style.overflow).toBe('hidden');

    rerender(
      <>
        <Layer open onEscape={escapes.bottom} />
        <Layer open={false} onEscape={escapes.middle} />
        <Layer open={false} onEscape={escapes.top} />
      </>
    );
    await user.keyboard('{Escape}');
    expect(escapes.bottom).toHaveBeenCalledTimes(1);

    rerender(
      <>
        <Layer open={false} onEscape={escapes.bottom} />
        <Layer open={false} onEscape={escapes.middle} />
        <Layer open={false} onEscape={escapes.top} />
      </>
    );
    expect(document.body.style.overflow).toBe('');
  });

  it('leaves the top overlay in charge when the one under it closes first', async () => {
    const user = userEvent.setup();
    const bottom = vi.fn();
    const middle = vi.fn();
    const top = vi.fn();

    const { rerender } = render(
      <>
        <Layer open onEscape={bottom} />
        <Layer open onEscape={middle} />
        <Layer open onEscape={top} />
      </>
    );
    rerender(
      <>
        <Layer open onEscape={bottom} />
        <Layer open={false} onEscape={middle} />
        <Layer open onEscape={top} />
      </>
    );

    await user.keyboard('{Escape}');
    // Removing a middle token must not evict the top one from the stack.
    expect(top).toHaveBeenCalledTimes(1);
    expect(bottom).not.toHaveBeenCalled();
    expect(document.body.style.overflow).toBe('hidden');

    rerender(
      <>
        <Layer open={false} onEscape={bottom} />
        <Layer open={false} onEscape={middle} />
        <Layer open={false} onEscape={top} />
      </>
    );
    expect(document.body.style.overflow).toBe('');
  });

  it('a re-rendered overlay keeps its place under the one stacked over it', async () => {
    const user = userEvent.setup();
    const bottom = vi.fn();
    const top = vi.fn();

    // Fresh handler on every render, the way WorkspaceSettingsModal's
    // `handleClose` is rebuilt whenever a section's dirty flag flips while the
    // global Settings modal sits over it: the identity moves, the overlay stays put.
    const { rerender } = render(
      <>
        <Layer open onEscape={() => bottom()} />
        <Layer open onEscape={top} />
      </>
    );
    rerender(
      <>
        <Layer open onEscape={() => bottom()} />
        <Layer open onEscape={top} />
      </>
    );

    await user.keyboard('{Escape}');
    // A new handler must not re-register the bottom overlay on top of the
    // one still open above it.
    expect(top).toHaveBeenCalledTimes(1);
    expect(bottom).not.toHaveBeenCalled();

    rerender(
      <>
        <Layer open={false} onEscape={() => bottom()} />
        <Layer open={false} onEscape={top} />
      </>
    );
    expect(document.body.style.overflow).toBe('');
  });

  it('lets a non-modal layer take Escape without locking or unlocking the page', async () => {
    const user = userEvent.setup();
    const modalEscape = vi.fn();
    const dropdownEscape = vi.fn();

    const { rerender } = render(
      <>
        <Layer open onEscape={modalEscape} />
        <Layer open={false} onEscape={dropdownEscape} options={{ lockScroll: false }} />
      </>
    );
    rerender(
      <>
        <Layer open onEscape={modalEscape} />
        <Layer open onEscape={dropdownEscape} options={{ lockScroll: false }} />
      </>
    );

    await user.keyboard('{Escape}');
    expect(dropdownEscape).toHaveBeenCalledTimes(1);
    expect(modalEscape).not.toHaveBeenCalled();

    rerender(
      <>
        <Layer open onEscape={modalEscape} />
        <Layer open={false} onEscape={dropdownEscape} options={{ lockScroll: false }} />
      </>
    );
    // The dropdown closing must not unlock the modal still underneath it.
    expect(document.body.style.overflow).toBe('hidden');

    rerender(
      <>
        <Layer open={false} onEscape={modalEscape} />
        <Layer open={false} onEscape={dropdownEscape} options={{ lockScroll: false }} />
      </>
    );
    expect(document.body.style.overflow).toBe('');
  });

  it('never locks the page for a non-modal layer on its own', () => {
    const { unmount } = render(<Layer open onEscape={vi.fn()} options={{ lockScroll: false }} />);
    expect(document.body.style.overflow).toBe('');
    unmount();
    expect(document.body.style.overflow).toBe('');
  });
});
