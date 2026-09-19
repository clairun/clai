import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import IntervalSelect from './IntervalSelect';
import { useOverlayLayer } from '../../hooks/useOverlayLayer';

/**
 * The arrangement that matters: this dropdown only ever opens inside a modal
 * (WorkspaceSettingsModal's Schedule section), and its popover portals to
 * <body> above it. Both listen for Escape on `document`.
 */
const HostModal = ({ onClose, children }: { onClose: () => void; children: React.ReactNode }) => {
  useOverlayLayer(true, onClose);
  return <div>{children}</div>;
};

const renderInModal = () => {
  const onClose = vi.fn();
  const onChange = vi.fn();
  render(
    <HostModal onClose={onClose}>
      <IntervalSelect value={30} onChange={onChange} />
    </HostModal>
  );
  return { onClose, onChange };
};

describe('IntervalSelect inside a modal', () => {
  it('gives Escape to the open dropdown, not to the modal hosting it', async () => {
    const user = userEvent.setup();
    const { onClose } = renderInModal();

    await user.click(screen.getByRole('button', { name: /30 minutes/ }));
    expect(screen.getByRole('listbox')).toBeInTheDocument();

    await user.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).toBeNull();
    // One Escape, one dismissal: the modal underneath is still open.
    expect(onClose).not.toHaveBeenCalled();

    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('closes on Escape with an option focused, not only the trigger', async () => {
    const user = userEvent.setup();
    const { onClose } = renderInModal();

    await user.click(screen.getByRole('button', { name: /30 minutes/ }));
    (screen.getByRole('option', { name: '1 hour' }) as HTMLElement).focus();

    await user.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('does not lock the page behind it', async () => {
    const user = userEvent.setup();
    render(<IntervalSelect value={30} onChange={vi.fn()} />);

    await user.click(screen.getByRole('button', { name: /30 minutes/ }));
    // A dropdown is not a modal: the page under it keeps scrolling.
    expect(document.body.style.overflow).toBe('');
  });
});
